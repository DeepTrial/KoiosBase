//! Ingest pipeline: raw/ + wiki/ → .index/ (§5) — mirrors ingest/pipeline.py.
//!
//! Two invariants:
//!   * graph edges are derived purely from Markdown wikilinks (§5.2), never
//!     from the db;
//!   * **derived pages ARE indexed.** Earlier drafts of this module claimed the
//!     opposite ("derived pages are NEVER indexed"), but `cmd_index` walks
//!     `wiki/` wholesale, so `sources/` and `entities/` rows land in the block
//!     table alongside everything else. The "never" rule was once correct as a
//!     guard against self-feeding, and the fear behind it still is — but the
//!     self-feeding is now prevented where it actually belongs: the compiler's
//!     input set is pinned to `layer='raw'` (see `compile`), so indexing the
//!     compiler's output cannot grow the corpus. Content hashes make ingest
//!     idempotent regardless (measured: consecutive rebuilds are stable).
//!     Excluding derived pages instead starved channel ① (§6.1, wiki-first) of
//!     the wiki rows it exists to prefer, and made the two shells disagree on
//!     block counts. When behaviour and the docstring disagree, the docstring
//!     is the bug — but fix BOTH, because a reader who finds this file first
//!     will otherwise re-break it.

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use std::path::Path;

use crate::pipeline::Ev;

static WIKILINK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|[^\]]*)?\]\]").unwrap());

/// dispatch by extension (§5.1: a new format = one new adapter).
pub fn is_supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|x| x.to_str())
            .map(|s| s.to_lowercase())
            .as_deref(),
        Some("md") | Some("markdown") | Some("pdf")
    )
}

/// True for pages the tool regenerates (v0.2 sources/, v0.3 entities/).
pub fn is_derived_page(path: &Path, base: &Path) -> bool {
    let rel = match path.strip_prefix(base) {
        Ok(r) => r,
        Err(_) => return false,
    };
    matches!(
        rel.components().next().and_then(|c| c.as_os_str().to_str()),
        Some("sources") | Some("entities")
    )
}

/// Walk a layer directory in sorted order, skipping .git/.index.
pub fn iter_docs(base: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            let skip = p
                .components()
                .any(|c| c.as_os_str() == ".git" || c.as_os_str() == ".index");
            if skip {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() && is_supported(&p) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Extract wikilink targets from one block's raw text.
pub fn wikilinks(raw: &str) -> Vec<String> {
    WIKILINK_RE
        .captures_iter(raw)
        .filter_map(|m| m.get(1).map(|g| g.as_str().trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Insert graph edges for every wikilink found in these blocks (§5.2).
/// Deduped by (src,dst,kind) exactly as the Python `seen_links` set does.
/// Structural edges for corpora that have no `[[wikilink]]` at all.
///
/// §5.2 lists four edge sources, but only explicit wikilinks were ever
/// written -- so on card-style Markdown the `links` table is empty (measured:
/// zero rows) and PPR contributes nothing. The `graph` channel then returns
/// exactly what BM25 returned, which is not a graph signal, it is a no-op with
/// extra steps.
///
/// These two are the cheap half of §5.2's list: blocks sharing a section, and
/// blocks sharing a document. Both are derivable from Markdown structure
/// alone, so they hold regardless of whether anyone wrote a link.
pub fn add_structural_edges(conn: &Connection) -> rusqlite::Result<usize> {
    let mut n = 0usize;
    // Same section: the strongest structural signal short of an explicit link.
    let mut stmt = conn.prepare(
        "SELECT section_id, group_concat(id) FROM blocks \
         WHERE section_id IS NOT NULL AND section_id <> '' \
         GROUP BY section_id HAVING count(*) > 1",
    )?;
    let groups: Vec<Vec<String>> = stmt
        .query_map([], |r| {
            let joined: String = r.get::<_, String>(1).unwrap_or_default();
            Ok(joined.split(',').map(|s| s.to_string()).collect())
        })?
        .flatten()
        .collect();
    for g in groups {
        for a in g.iter() {
            let w = weight_for_block(conn, a, 0.5);
            for b in g.iter() {
                if a == b {
                    continue;
                }
                conn.execute(
                    "INSERT INTO links(src,dst,kind,weight) VALUES(?,?,?,?)",
                    params![a, b, "same-section", w],
                )?;
                n += 1;
            }
        }
    }
    // Same document, weaker: it keeps navigation across sections possible.
    let mut stmt2 = conn.prepare(
        "SELECT doc_path, group_concat(id) FROM blocks \
         WHERE doc_path IS NOT NULL AND doc_path <> '' \
         GROUP BY doc_path HAVING count(*) > 1",
    )?;
    let docs: Vec<Vec<String>> = stmt2
        .query_map([], |r| {
            let joined: String = r.get::<_, String>(1).unwrap_or_default();
            Ok(joined.split(',').map(|s| s.to_string()).collect())
        })?
        .flatten()
        .collect();
    for g in docs {
        for a in g.iter() {
            let w = weight_for_block(conn, a, 0.2);
            for b in g.iter() {
                if a == b {
                    continue;
                }
                conn.execute(
                    "INSERT INTO links(src,dst,kind,weight) VALUES(?,?,?,?)",
                    params![a, b, "same-doc", w],
                )?;
                n += 1;
            }
        }
    }
    Ok(n)
}

/// Structural-edge weight, scaled by how much the corpus has been verified.
///
/// §5.4 is not optional here: draft pages must contribute NO graph weight, or a
/// machine-generated page gains authority simply by existing and the
/// self-reinforcement loop the section exists to cut comes back through the
/// structural edges. Raw documents are human-authored, so they are the
/// reference point; an all-draft corpus stays weightless.
/// Weight of one structural edge, given the block it starts from.
///
/// §5.4 applies here exactly as it does to wikilinks: a page nobody has
/// confirmed must contribute no graph weight, or a machine-generated page
/// gains authority merely by existing and the self-reinforcement loop that
/// section exists to cut returns through the structural edges. So the edge
/// inherits its source block's confidence: raw documents carry it, draft
/// derived pages do not.
fn weight_for_block(conn: &Connection, block_id: &str, base: f64) -> f64 {
    let conf: String = conn
        .query_row(
            "SELECT CASE WHEN COALESCE(layer,'raw')='raw' THEN 'high' ELSE 'draft' END \
             FROM blocks WHERE id=?",
            [block_id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "draft".to_string());
    base * crate::compile::edge_weight(&conf)
}

pub fn add_wikilink_edges(
    conn: &Connection,
    blocks: &[Ev],
    seen: &mut std::collections::HashSet<(String, String, String)>,
) -> rusqlite::Result<()> {
    for b in blocks {
        for link in wikilinks(&b.raw) {
            let key = (b.id.clone(), link.clone(), "wikilink".to_string());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            conn.execute(
                "INSERT INTO links(src,dst,kind,weight) VALUES(?,?,?,?)",
                params![b.id, link, "wikilink", 1.0f64],
            )?;
        }
    }
    Ok(())
}
