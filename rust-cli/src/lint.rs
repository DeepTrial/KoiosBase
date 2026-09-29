//! lint gardener (§9.3) — mirrors koiosbase/lint/gardener.py.
//!
//! v0.1 ships the L1 programmatic checks (§10.3): no model at all, therefore
//! zero self-verification risk (P7 prefers the lowest level that can do the job).

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};

use crate::state::now_ts_days;

static WIKILINK_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[\[([^\]]+)\]\]").unwrap());
static NUM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\d+(?:\.\d+)?").unwrap());
static TTL_DAYS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d+)\s*d$").unwrap());

pub fn check_broken_links(conn: &Connection) -> Vec<String> {
    let mut bad = Vec::new();
    let mut stmt = match conn.prepare("SELECT src,dst FROM links WHERE kind='wikilink'") {
        Ok(s) => s,
        Err(_) => return bad,
    };
    let rows: Vec<(String, String)> = match stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))) {
        Ok(m) => m.filter_map(Result::ok).collect(),
        Err(_) => return bad,
    };
    for (src, dst) in rows {
        let p1 = format!("%{dst}%");
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM blocks WHERE id LIKE ? OR doc_path LIKE ? LIMIT 1",
                params![p1, p1],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !exists {
            bad.push(format!("broken wikilink: {src} -> {dst}"));
        }
    }
    bad
}

/// Compile-layer pages whose factual lines cite nothing violate the citation
/// contract (P6) — surfaced here rather than at generation time.
pub fn check_unsourced_assertions(conn: &Connection) -> Vec<String> {
    let mut bad = Vec::new();
    let mut stmt = match conn.prepare("SELECT id,raw FROM blocks WHERE layer='wiki'") {
        Ok(s) => s,
        Err(_) => return bad,
    };
    let rows: Vec<(String, String)> = match stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))) {
        Ok(m) => m.filter_map(Result::ok).collect(),
        Err(_) => return bad,
    };
    for (id, raw) in rows {
        for line in raw.lines() {
            let line = line.trim();
            if !line.starts_with('-') && !line.starts_with('*') {
                continue;
            }
            let content = line.trim_start_matches(['-', '*', ' ']).trim();
            if content.is_empty() || WIKILINK_RE.is_match(content) {
                continue;
            }
            // Skip structural lines: headers, separators, table rows, code.
            let structural = content.starts_with(['#', '|', '`', '>'])
                || content.chars().all(|c| "-=*| ".contains(c));
            if structural {
                continue;
            }
            let snippet: String = content.chars().take(60).collect();
            bad.push(format!("unsourced assertion in {id}: {snippet}"));
        }
    }
    bad
}

fn fm_json(raw: &str) -> Option<serde_json::Value> {
    serde_json::from_str(raw).ok()
}

/// TTL expiry (§9.1): frontmatter ttl + valid_from means the claim is old.
pub fn check_expired_ttl(conn: &Connection) -> Vec<String> {
    let mut bad = Vec::new();
    let mut stmt = match conn.prepare("SELECT path,frontmatter FROM documents") {
        Ok(s) => s,
        Err(_) => return bad,
    };
    let rows: Vec<(String, String)> = match stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))) {
        Ok(m) => m.filter_map(Result::ok).collect(),
        Err(_) => return bad,
    };
    let today = now_ts_days();
    for (path, fm_s) in rows {
        let fm = match fm_json(&fm_s) {
            Some(v) => v,
            None => continue,
        };
        let ttl = match fm.get("ttl") {
            Some(v) => v,
            None => continue,
        };
        let valid_from = match fm.get("valid_from") {
            Some(v) => v,
            None => continue,
        };
        let ttl_s = ttl.to_string().trim_matches('"').to_string();
        let days: Option<i64> = if let Some(m) = TTL_DAYS_RE.captures(&ttl_s) {
            m[1].parse().ok()
        } else if ttl_s.chars().all(|c| c.is_ascii_digit()) && !ttl_s.is_empty() {
            ttl_s.parse().ok()
        } else {
            None
        };
        let Some(days) = days else { continue };
        let vf = valid_from.to_string().trim_matches('"').to_string();
        let start_day = match parse_ymd_days(&vf) {
            Some(d) => d,
            None => continue,
        };
        let age = today - start_day;
        if age > days {
            bad.push(format!("TTL expired: {path} ({age}d > {days}d)"));
        }
    }
    bad
}

fn parse_ymd_days(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let y: i64 = parts[0].parse().ok()?;
    let m: i64 = parts[1].parse().ok()?;
    let d: i64 = parts[2].parse().ok()?;
    Some(crate::state::days_from_civil(y, m as u64, d as u64))
}

/// Report pages flagged stale by cascade propagation (§8.3).
pub fn check_stale_pages(conn: &Connection) -> Vec<String> {
    if conn.execute("SELECT 1 FROM page_state LIMIT 1", []).is_err() {
        return Vec::new();
    }
    let mut stmt = match conn.prepare("SELECT page_path,reason FROM page_state WHERE stale=1") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows: Vec<(String, String)> = match stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))) {
        Ok(m) => m.filter_map(Result::ok).collect(),
        Err(_) => return Vec::new(),
    };
    rows.into_iter()
        .map(|(p, r)| format!("stale page: {p} ({r})"))
        .collect()
}

/// Open conflicts parked in 「矛盾与未决」 must not be silently forgotten.
pub fn check_conflict_markers(conn: &Connection) -> Vec<String> {
    let mut out = Vec::new();
    let mut stmt = match conn.prepare(
        "SELECT id,raw FROM blocks WHERE layer='wiki' AND raw LIKE '%矛盾与未决%'",
    ) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let rows: Vec<(String, String)> = match stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))) {
        Ok(m) => m.filter_map(Result::ok).collect(),
        Err(_) => return out,
    };
    for (id, raw) in rows {
        let seg = raw.split("矛盾与未决").nth(1).unwrap_or("");
        for line in seg.lines() {
            let s = line.trim();
            if s.starts_with('-') && s.contains('⚠') {
                let snippet: String = s.chars().take(70).collect();
                out.push(format!("open conflict in {id}: {snippet}"));
            }
        }
    }
    out
}

/// wiki pages that nothing links to and that cite nothing (§9.3).
pub fn check_orphan_pages(conn: &Connection) -> Vec<String> {
    let mut out = Vec::new();
    let mut stmt = match conn.prepare("SELECT id,raw,doc_path FROM blocks WHERE layer='wiki'") {
        Ok(s) => s,
        Err(_) => return out,
    };
    let rows: Vec<(String, String, String)> =
        match stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))) {
            Ok(m) => m.filter_map(Result::ok).collect(),
            Err(_) => return out,
        };
    for (_id, raw, doc_path) in rows {
        if !raw.contains("[[") {
            out.push(format!("orphan page (no links at all): {doc_path}"));
        }
    }
    out
}

pub type Findings = Vec<(&'static str, Vec<String>)>;

/// All model-free checks (§10.3 L1). Order matches Python's run_l1 dict so the
/// printed output lines up line-for-line between the two shells.
pub fn run_l1(conn: &Connection) -> Findings {
    vec![
        ("broken_links", check_broken_links(conn)),
        ("unsourced_assertions", check_unsourced_assertions(conn)),
        ("expired_ttl", check_expired_ttl(conn)),
        ("stale_pages", check_stale_pages(conn)),
        ("open_conflicts", check_conflict_markers(conn)),
        ("orphan_pages", check_orphan_pages(conn)),
    ]
}

pub fn cmd_lint(vault: &std::path::Path) -> rusqlite::Result<i32> {
    let conn = crate::connect(vault)?;
    let findings = run_l1(&conn);
    let total: usize = findings.iter().map(|(_, v)| v.len()).sum();
    for (kind, items) in &findings {
        for it in items {
            println!("[{kind}] {it}");
        }
    }
    println!("---- lint: {total} finding(s)");
    Ok(if total > 0 { 1 } else { 0 })
}

/// Cross-family judgement (§10.3 L2) — mirrors koiosbase/generation/judge.py.
///
/// Deterministic placeholder: only *programmatically checkable* relations
/// (numbers) are decided; everything else honestly returns 'unknown' rather
/// than pretending to understand modality or negation (§8.4).
pub fn cross_judge(claim: &str, evidence: &str) -> &'static str {
    let cn: Vec<String> = NUM_RE
        .find_iter(claim)
        .map(|m| m.as_str().to_string())
        .collect();
    let en: Vec<String> = NUM_RE
        .find_iter(evidence)
        .map(|m| m.as_str().to_string())
        .collect();
    if cn.is_empty() || en.is_empty() {
        return "unknown";
    }
    if cn.iter().any(|c| en.contains(c)) {
        "entailed"
    } else {
        "contradicted"
    }
}
