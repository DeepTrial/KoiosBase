//! Retrieval channels (§6.1) mirroring koiosbase/retrieval/{router,channels}.py.
//!
//! Kept deliberately faithful to the Python behaviour, including the two
//! non-obvious details that were found by execution there:
//!   * `unicode61` collapses a contiguous CJK run into ONE token, so BM25 needs
//!     the per-character padded form (see cjk_pad) to match Chinese at all;
//!   * Personalized PageRank must redistribute dangling mass by the
//!     PERSONALIZATION vector, not uniformly — otherwise sink nodes outrank the
//!     seeds and the "personalized" part is inverted.

use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

pub const ALPHA: f64 = 1.0;
pub const BETA: f64 = 1.0;
pub const GAMMA: f64 = 0.12;
pub const RRF_K: f64 = 60.0;

pub fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// Insert spaces between CJK characters. Mirrors Python's cjk_pad().
pub fn cjk_pad(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if is_cjk(c) {
            out.push(' ');
            out.push(c);
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

pub fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| !(c.is_alphanumeric() || is_cjk(c)))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Channel ② BM25 over the FTS mirror. AND first (precision), then OR (recall).
pub fn fts_search(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> rusqlite::Result<Vec<(String, f64)>> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut padded: Vec<String> = Vec::new();
    for t in &terms {
        if t.chars().any(is_cjk) {
            // whole-phrase first: per-character padding is only a fallback, since
            // matching on one common char (年) breaks the refusal contract.
            padded.push(
                t.chars()
                    .collect::<Vec<_>>()
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        } else {
            padded.push(t.clone());
        }
    }
    let quoted: Vec<String> = padded
        .iter()
        .take(32)
        .map(|p| format!("\"{}\"", p))
        .collect();
    let mut chars: Vec<String> = Vec::new();
    for p in &padded {
        if p.chars().any(is_cjk) {
            chars.extend(p.chars().filter(|c| is_cjk(*c)).map(|c| c.to_string()));
        } else {
            chars.push(p.clone());
        }
    }
    let char_expr: String = chars
        .iter()
        .take(64)
        .map(|c| format!("\"{}\"", c))
        .collect::<Vec<_>>()
        .join(" OR ");
    for expr in [quoted.join(" AND "), quoted.join(" OR "), char_expr.clone()] {
        let sql = "SELECT id, bm25(blocks_fts) AS score FROM blocks_fts \
                   WHERE blocks_fts MATCH ? ORDER BY score LIMIT ?";
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rows: Vec<(String, f64)> = stmt
            .query_map(params![expr, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        if rows.is_empty() {
            continue;
        }
        let mut rows = rows;
        if expr == char_expr && chars.len() > 1 {
            // require >=2 distinct query characters in the raw text, or a single
            // ubiquitous character defeats the refusal contract.
            let need = std::cmp::min(2, chars.len());
            rows = rows
                .into_iter()
                .filter(|(id, _)| {
                    let raw: String = conn
                        .query_row("SELECT raw FROM blocks WHERE id=?", params![id], |r| {
                            r.get(0)
                        })
                        .unwrap_or_default();
                    chars.iter().filter(|c| raw.contains(c.as_str())).count() >= need
                })
                .collect();
        }
        if !rows.is_empty() {
            return Ok(rows);
        }
    }
    Ok(Vec::new())
}

/// Reciprocal Rank Fusion — the only safe merge for incomparable scores.
pub fn rrf_fuse(lists: &[Vec<(String, f64)>], weights: &[f64], k: f64) -> Vec<(String, f64)> {
    let mut fused: HashMap<String, f64> = HashMap::new();
    for (list, w) in lists.iter().zip(weights.iter()) {
        for (rank, (id, _)) in list.iter().enumerate() {
            *fused.entry(id.clone()).or_insert(0.0) += w * (1.0 / (k + rank as f64 + 1.0));
        }
    }
    let mut out: Vec<(String, f64)> = fused.into_iter().collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

pub fn load_graph(conn: &Connection) -> rusqlite::Result<HashMap<String, Vec<(String, f64)>>> {
    let mut stmt = conn.prepare("SELECT src,dst,weight FROM links")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;
    let mut adj: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    for r in rows.flatten() {
        adj.entry(r.0).or_default().push((r.1, r.2));
    }
    Ok(adj)
}

/// Personalized PageRank over the <=2-hop neighbourhood of the seeds (§6.3).
pub fn personalized_pagerank(
    seeds: &[String],
    adj: &HashMap<String, Vec<(String, f64)>>,
    damping: f64,
    iterations: usize,
) -> HashMap<String, f64> {
    if seeds.is_empty() || adj.is_empty() {
        return HashMap::new();
    }
    let mut frontier: HashSet<String> = seeds.iter().cloned().collect();
    for _ in 0..2 {
        let mut next = frontier.clone();
        for n in frontier.iter() {
            if let Some(edges) = adj.get(n) {
                for (dst, _) in edges {
                    next.insert(dst.clone());
                }
            }
        }
        frontier = next;
        if frontier.len() > 500 {
            break;
        }
    }
    let nodes: Vec<String> = frontier.iter().cloned().collect();
    if nodes.is_empty() {
        return HashMap::new();
    }
    let personal: HashMap<String, f64> = seeds
        .iter()
        .filter(|s| frontier.contains(*s))
        .map(|s| (s.clone(), 1.0 / seeds.len() as f64))
        .collect();
    if personal.is_empty() {
        return HashMap::new();
    }
    let out_w: HashMap<String, f64> = nodes
        .iter()
        .map(|n| {
            let w = adj
                .get(n)
                .map(|e| e.iter().map(|(_, x)| x).sum::<f64>())
                .unwrap_or(0.0);
            (n.clone(), w)
        })
        .collect();

    let mut pr: HashMap<String, f64> = nodes
        .iter()
        .map(|n| (n.clone(), 1.0 / nodes.len() as f64))
        .collect();
    for _ in 0..iterations {
        let mut new: HashMap<String, f64> = nodes
            .iter()
            .map(|n| {
                (
                    n.clone(),
                    (1.0 - damping) * personal.get(n).cloned().unwrap_or(0.0),
                )
            })
            .collect();
        let mut dangling = 0.0;
        for n in &nodes {
            if out_w.get(n).cloned().unwrap_or(0.0) <= 0.0 {
                dangling += pr.get(n).cloned().unwrap_or(0.0);
            }
        }
        for n in &nodes {
            let total = out_w.get(n).cloned().unwrap_or(0.0);
            if total > 0.0 {
                if let Some(edges) = adj.get(n) {
                    for (dst, w) in edges {
                        if let Some(v) = new.get_mut(dst) {
                            *v += damping * pr.get(n).cloned().unwrap_or(0.0) * (w / total);
                        }
                    }
                }
            }
        }
        // dangling mass goes back through the personalization vector
        if dangling > 0.0 {
            for n in &nodes {
                if let Some(v) = new.get_mut(n) {
                    *v += damping * dangling * personal.get(n).cloned().unwrap_or(0.0);
                }
            }
        }
        let diff: f64 = nodes.iter().map(|n| (new[n] - pr[n]).abs()).sum();
        pr = new;
        if diff < 1e-6 {
            break;
        }
    }
    pr
}

/// Channel ② + ③: BM25 fused with the PPR graph bonus.
pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    limit: usize,
    use_graph: bool,
) -> rusqlite::Result<Vec<(String, f64)>> {
    let mut fused = rrf_fuse(&[fts_search(conn, query, limit * 3)?], &[BETA], RRF_K);
    if use_graph {
        let adj = load_graph(conn)?;
        if !adj.is_empty() {
            let seeds: Vec<String> = fused.iter().take(50).map(|(id, _)| id.clone()).collect();
            let pr = personalized_pagerank(&seeds, &adj, 0.85, 20);
            if !pr.is_empty() {
                fused = fused
                    .into_iter()
                    .map(|(id, s)| {
                        let bonus = GAMMA * pr.get(&id).cloned().unwrap_or(0.0) * 10.0;
                        (id, s + bonus)
                    })
                    .collect();
                fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            }
        }
    }
    fused.truncate(limit);
    Ok(fused)
}

/// Channel ①: pick sections whose navigational summary matches, return their blocks.
pub fn tree_search(
    conn: &Connection,
    query: &str,
    top_sections: usize,
) -> rusqlite::Result<Vec<String>> {
    let terms: HashSet<String> = query_terms(query)
        .iter()
        .flat_map(|t| {
            cjk_pad(t)
                .split_whitespace()
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
        })
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT id,title,summary FROM sections")?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?
        .filter_map(Result::ok)
        .collect();
    let mut scored: Vec<(String, f64)> = Vec::new();
    for (id, title, summary) in rows {
        let hay: HashSet<String> = cjk_pad(&format!("{title} {summary}"))
            .split_whitespace()
            .map(|s| s.to_lowercase())
            .collect();
        let hits = hay.intersection(&terms).count();
        if hits > 0 {
            scored.push((id, hits as f64 / terms.len() as f64));
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut ids = Vec::new();
    for (sid, _) in scored.into_iter().take(top_sections) {
        let mut s2 = conn.prepare("SELECT id FROM blocks WHERE section_id=? ORDER BY ordinal")?;
        let block_ids: Vec<String> = s2
            .query_map(params![sid], |r| r.get(0))?
            .filter_map(Result::ok)
            .collect();
        ids.extend(block_ids);
    }
    Ok(ids)
}

/// Channel ④: whole corpus when small enough; empty when over the threshold.
pub fn full_corpus(conn: &Connection, max_chars: usize) -> rusqlite::Result<Vec<String>> {
    let size: i64 = conn.query_row("SELECT COALESCE(SUM(LENGTH(raw)),0) FROM blocks", [], |r| {
        r.get(0)
    })?;
    if size as usize > max_chars {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT id FROM blocks ORDER BY doc_path, ordinal")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .filter_map(Result::ok)
        .collect();
    Ok(ids)
}

/// Grader verdict (§6.2): enough | missing | evidence_absent.
pub fn grade(question: &str, joined: &str) -> &'static str {
    if joined.is_empty() {
        return "evidence_absent";
    }
    let lower = joined.to_lowercase();
    let hits = question
        .chars()
        .collect::<HashSet<char>>()
        .iter()
        .filter(|c| {
            if c.is_alphanumeric() {
                lower.contains(&c.to_lowercase().to_string())
            } else if is_cjk(**c) {
                lower.contains(**c)
            } else {
                false
            }
        })
        .count();
    if hits == 0 {
        "missing"
    } else {
        "enough"
    }
}
