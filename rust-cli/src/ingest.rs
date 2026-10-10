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
