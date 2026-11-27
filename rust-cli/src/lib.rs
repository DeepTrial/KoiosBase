//! Library facade for the `koios` binary.
//!
//! The crate used to be binary-only, so every module was unreachable from
//! `tests/` — the index/search pipeline had exactly ONE unit test and nothing
//! guarding the ingest behaviour the Python suite covers. Re-exporting the
//! modules lets integration tests drive real commands against a temp vault.
//!
//! `src/main.rs` keeps the CLI (clap) and imports this facade, so both targets
//! compile the same logic and cannot drift.

pub mod acl;
pub mod cache;
pub mod compile;
pub mod embed;
pub mod ingest;
pub mod lint;
pub mod llm;
pub mod mcp;
pub mod pdf;
pub mod pipeline;
pub mod retrieval;
pub mod state;
pub mod studio;

use rusqlite::Connection;
use std::path::Path;

/// Mirrors koiosbase/index/schema.py SCHEMA + state/model.py SCHEMA_EXTRA.
///
/// MOVED here from main.rs so there is exactly one definition: duplicating it
/// risks the lib and the binary disagreeing about the derived format (P1).
/// Mirrors koiosbase/index/schema.py SCHEMA + state/model.py SCHEMA_EXTRA.
/// Every statement must stay in lockstep with the Python side or one shell
/// writes a db the other cannot read (P1: the index is derived, but both
/// shells must agree on what "derived" means).
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS documents (
  path TEXT PRIMARY KEY, title TEXT, frontmatter TEXT, hash TEXT);
CREATE TABLE IF NOT EXISTS sections (
  id TEXT PRIMARY KEY, doc_path TEXT, parent_id TEXT, title TEXT,
  summary TEXT, ordinal INTEGER, layer TEXT NOT NULL DEFAULT 'raw');
CREATE TABLE IF NOT EXISTS blocks (
  id TEXT PRIMARY KEY, doc_path TEXT, section_id TEXT, type TEXT,
  breadcrumb TEXT, raw TEXT, hash TEXT, page_range TEXT, meta TEXT,
  ordinal INTEGER NOT NULL DEFAULT 0, layer TEXT NOT NULL DEFAULT 'raw');
CREATE VIRTUAL TABLE IF NOT EXISTS blocks_fts USING fts5(
  id, breadcrumb, raw, tokenize='unicode61');
CREATE TABLE IF NOT EXISTS links (
  src TEXT, dst TEXT, kind TEXT, weight REAL);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
CREATE INDEX IF NOT EXISTS idx_blocks_section ON blocks(section_id);
-- Per-document writes issue several `WHERE doc_path=?` deletes and selects
-- per doc. Without this index each was a full table scan, so indexing N docs
-- cost O(N^2) row visits -- measured: the wiki-layer write phase was 4x
-- slower than the raw phase on the same doc count purely from table growth.
-- This, not CPU parallelism, was the real bottleneck on large vaults.
CREATE INDEX IF NOT EXISTS idx_blocks_doc ON blocks(doc_path);
CREATE INDEX IF NOT EXISTS idx_sections_doc ON sections(doc_path);
CREATE INDEX IF NOT EXISTS idx_links_src ON links(src);
CREATE INDEX IF NOT EXISTS idx_links_dst ON links(dst);
CREATE TABLE IF NOT EXISTS block_state (
  block_id TEXT PRIMARY KEY,
  state TEXT NOT NULL DEFAULT 'active',
  reason TEXT,
  updated TEXT
);
CREATE TABLE IF NOT EXISTS page_state (
  page_path TEXT PRIMARY KEY,
  stale INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL DEFAULT 'active',
  reason TEXT,
  updated TEXT
);
"#;

/// Open (creating if needed) the vault's derived index — the same path every
/// subcommand uses, so tests and CLI cannot drift onto different databases.
pub fn connect(vault: &Path) -> rusqlite::Result<Connection> {
    let dir = vault.join(".index");
    std::fs::create_dir_all(&dir).ok();
    let conn = Connection::open(dir.join("tree.db"))?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Add columns introduced after the first release — mirrors index/schema.py
/// `_migrate()`.
///
/// `CREATE TABLE IF NOT EXISTS` is a no-op on an existing table, so a vault
/// built by an older version keeps its old column set and every later query
/// against a new column fails with "no such column". These ALTERs make old
/// vaults usable without requiring a rebuild (rebuild stays available, not
/// mandatory). Rust shipped without this, so any vault indexed before the
/// layer/ordinal columns landed was simply unreadable from the Rust shell.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for (table, col, ddl) in [
        (
            "blocks",
            "layer",
            "ALTER TABLE blocks ADD COLUMN layer TEXT NOT NULL DEFAULT 'raw'",
        ),
        (
            "blocks",
            "ordinal",
            "ALTER TABLE blocks ADD COLUMN ordinal INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "sections",
            "layer",
            "ALTER TABLE sections ADD COLUMN layer TEXT NOT NULL DEFAULT 'raw'",
        ),
    ] {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(Result::ok)
            .collect();
        // Empty means the table does not exist yet (fresh db) — nothing to add.
        if !cols.is_empty() && !cols.iter().any(|c| c == col) {
            conn.execute(ddl, [])?;
        }
    }
    Ok(())
}

use rusqlite::params;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// Stable hash of normalized text — mirrors koiosbase/core/block.py.
///
/// Whitespace runs collapse to a single space and the result is trimmed before
/// hashing, then truncated to 16 hex chars. This value drives incremental
/// re-indexing (§4.1), so a mismatch means every rebuild looks like every block
/// changed — or worse, a real edit looks like a no-op.
pub fn content_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut norm = String::with_capacity(text.len());
    let mut prev_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            prev_space = true;
            continue;
        }
        if prev_space && !norm.is_empty() {
            norm.push(' ');
        }
        prev_space = false;
        norm.push(c);
    }
    let norm = norm.trim().to_string();
    let mut h = Sha256::new();
    h.update(norm.as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}

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

/// Apply each document's declared frontmatter state to all of its blocks.
///
/// Runs after the whole ingest pass so it also covers the derived `sources/`
/// page, which is produced later -- otherwise a superseded card keeps
/// answering through its summary after the source was hidden.
fn apply_declared_states(conn: &Connection) -> rusqlite::Result<()> {
    // Clear first, so a card edited back to active is not left superseded:
    // state is derived from Markdown, never sticky.
    conn.execute(
        "DELETE FROM block_state WHERE reason LIKE 'frontmatter state:%'",
        [],
    )?;
    let mut q = conn.prepare(
        "SELECT path, frontmatter FROM documents WHERE frontmatter IS NOT NULL \
         AND frontmatter <> ''",
    )?;
    let rows: Vec<(String, String)> = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .flatten()
        .collect();
    for (path, fm_json) in rows {
        let Some(st) = fm_state(&fm_json) else {
            continue;
        };
        if st == "active" {
            continue;
        }
        for prefix in [path.clone(), format!("sources/{path}")] {
            let mut ids: Vec<String> = Vec::new();
            {
                let mut s = conn.prepare("SELECT id FROM blocks WHERE doc_path=?")?;
                let m = s.query_map([&prefix], |r| r.get::<_, String>(0))?;
                for r in m.flatten() {
                    ids.push(r);
                }
            }
            for id in ids {
                crate::state::set_block_state(conn, &id, &st, "frontmatter state:")
                    .map_err(rusqlite::Error::InvalidParameterName)?;
            }
        }
    }
    Ok(())
}

/// One parsed block -- the parallel phase's product, with no DB handle.
pub struct ParsedBlock {
    pub id: String,
    pub section_id: String,
    pub kind: &'static str,
    pub breadcrumb: String,
    pub body: String,
    pub hash: String,
    pub ordinal: usize,
}

/// One parsed section row.
pub struct ParsedSection {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub ordinal: i64,
}

/// A fully parsed document.
///
/// Parsing is pure CPU work with zero DB access, so `index` runs it across
/// all cores (rayon sizes its pool to `available_parallelism`, deliberately
/// no config knob); the serial phase (`write_parsed_doc`) then applies the
/// rows. The old loop read, parsed and wrote one file at a time -- measured
/// single-threaded on machines where every other core was idle.
pub struct ParsedDoc {
    pub rel: String,
    pub title: String,
    pub fm_stored: String,
    pub fm_declared_state: Option<String>,
    pub layer: String,
    pub blocks: Vec<ParsedBlock>,
    pub sections: Vec<ParsedSection>,
    /// Blocks as evidence rows for the wikilink pass. The old code inserted
    /// them and then re-SELECTed them one query per file; they are already in
    /// memory here.
    pub evs: Vec<pipeline::Ev>,
}

/// Parse one document. Pure -- no DB -- so it is safe to run in parallel.
pub fn parse_doc(doc_path: &str, text: &str, layer: &str) -> ParsedDoc {
    let (fm, body) = split_frontmatter(text);
    let title = fm_title(&fm).unwrap_or_else(|| doc_path.to_string());
    let mut blocks: Vec<ParsedBlock> = Vec::new();
    let mut sections: Vec<ParsedSection> = Vec::new();
    let mut ordinal = 0usize;
    // Heading stack mirrors koiosbase/parsers/markdown.py: (level, path_titles).
    // Block ids must be byte-identical to Python's
    //   f"{doc_path}#{'/'.join(section_titles)}/{ordinal}"
    // or the two shells write different trees into the same schema and eval
    // results cannot be compared (P1: index is derived, Markdown is truth).
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut section_no = 0i64;

    let flush = |pending: &mut Vec<String>,
                 sec_path: &str,
                 blocks: &mut Vec<ParsedBlock>,
                 ordinal: &mut usize| {
        // Python's split_blocks() joins buf with "\n" then .strip() -- it does
        // NOT collapse interior newlines. Joining with " " here made multi-line
        // list blocks a single space-separated line, which then sat ONE
        // unsourced-assertion finding where Python reported three (lint walks
        // block lines), and made every derived page's raw differ.
        let body: String = pending
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        pending.clear();
        if body.is_empty() {
            return;
        }
        *ordinal += 1;
        // id matches Python exactly: "{doc}#{sec_path}/{ordinal}", or
        // "{doc}/{ordinal}" when there is no enclosing heading at all.
        let id = if sec_path.is_empty() {
            format!("{}/{}", doc_path, *ordinal)
        } else {
            format!("{}#{}/{}", doc_path, sec_path, *ordinal)
        };
        let sec_id = if sec_path.is_empty() {
            doc_path.to_string()
        } else {
            format!("{}#{}", doc_path, sec_path)
        };
        let crumb = if sec_path.is_empty() {
            title.clone()
        } else {
            format!(
                "{} > {}",
                title,
                sec_path.split('/').collect::<Vec<_>>().join(" > ")
            )
        };
        let kind = if body.contains('|') {
            "table"
        } else if body.lines().all(|l| {
            let s = l.trim();
            s.is_empty()
                || s.starts_with("- ")
                || s.starts_with("* ")
                || s.starts_with("+ ")
                || (s.chars().next().is_some_and(|c| c.is_ascii_digit()) && s.contains(". "))
        }) {
            // Python's split_blocks() tracks a list until the run breaks, so a
            // list flushes with type "list" regardless of length. Rust only had
            // paragraph/table, so every "- item" block was typed 'paragraph'.
            "list"
        } else {
            "paragraph"
        };
        // `hash` drives incremental re-indexing (§4.1): content_hash() is the
        // sha256 of whitespace-normalized text, first 16 hex chars.
        let haystack = format!("{}\n{}", crumb, body);
        blocks.push(ParsedBlock {
            id,
            section_id: sec_id,
            kind,
            breadcrumb: crumb.clone(),
            body,
            hash: content_hash(&haystack),
            ordinal: *ordinal,
        });
    };

    // sec_path: current "/"-joined heading titles; "" means no heading yet.
    let mut sec_path = String::new();
    for line in body.lines() {
        let st = line.trim();
        if st.is_empty() {
            flush(&mut pending, &sec_path, &mut blocks, &mut ordinal);
            continue;
        }
        if let Some(heading) = st.strip_prefix('#') {
            flush(&mut pending, &sec_path, &mut blocks, &mut ordinal);
            let hashes = st.chars().take_while(|c| *c == '#').count();
            let htitle = heading.trim_start_matches('#').trim().to_string();
            let level = hashes.min(6);
            while stack.last().map(|(l, _)| *l >= level).unwrap_or(false) {
                stack.pop();
            }
            stack.push((level, htitle.clone()));
            let titles: Vec<String> = stack.iter().map(|(_, t)| t.clone()).collect();
            sec_path = titles.join("/");
            // Python calls split_blocks() per section with start_ord=0, so the
            // ordinal restarts at 1 inside every section (not global).
            ordinal = 0;
            section_no += 1;
            let sid = format!("{}#{}", doc_path, sec_path);
            let parent_id: Option<String> = if stack.len() > 1 {
                let parent: Vec<String> = stack[..stack.len() - 1]
                    .iter()
                    .map(|(_, t)| t.clone())
                    .collect();
                Some(format!("{}#{}", doc_path, parent.join("/")))
            } else {
                None
            };
            sections.push(ParsedSection {
                id: sid,
                parent_id,
                title: htitle,
                ordinal: section_no,
            });
            continue;
        }
        pending.push(line.to_string());
    }
    flush(&mut pending, &sec_path, &mut blocks, &mut ordinal);

    let evs: Vec<pipeline::Ev> = blocks
        .iter()
        .map(|b| pipeline::Ev {
            id: b.id.clone(),
            raw: b.body.clone(),
            breadcrumb: b.breadcrumb.clone(),
            doc_path: doc_path.to_string(),
            channels: None,
            state: None,
        })
        .collect();

    ParsedDoc {
        rel: doc_path.to_string(),
        title,
        fm_stored: fm_for_storage(&fm),
        fm_declared_state: fm_state(&fm),
        layer: layer.to_string(),
        blocks,
        sections,
        evs,
    }
}

/// Apply one parsed document inside the serial write phase of `index`.
pub fn write_parsed_doc(conn: &Connection, doc: &ParsedDoc) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT OR REPLACE INTO documents(path,title,frontmatter,hash) VALUES(?,?,?,?)",
        params![doc.rel, doc.title, doc.fm_stored, ""],
    )?;

    // Re-indexing is not idempotent on its own. `INSERT OR REPLACE` refreshes
    // the rows a document still produces and leaves every other row for that
    // doc_path untouched -- so shortening a document keeps its old blocks
    // alive: measured, three paragraphs reduced to one left /2 and /3 in place
    // and `koios search` still returned the deleted text, which could then be
    // cited. Derived rows for this document are cleared first; §4.3 derived
    // pages are regenerated by the same pass.
    conn.execute(
        "DELETE FROM blocks_fts WHERE id IN (SELECT id FROM blocks WHERE doc_path=?)",
        [&doc.rel],
    )?;
    conn.execute(
        "DELETE FROM links WHERE src IN (SELECT id FROM blocks WHERE doc_path=?) \
         OR dst IN (SELECT id FROM blocks WHERE doc_path=?)",
        params![doc.rel, doc.rel],
    )?;
    conn.execute(
        "DELETE FROM block_state WHERE block_id IN (SELECT id FROM blocks WHERE doc_path=?)",
        [&doc.rel],
    )?;
    conn.execute("DELETE FROM blocks WHERE doc_path=?", [&doc.rel])?;
    conn.execute("DELETE FROM sections WHERE doc_path=?", [&doc.rel])?;

    for b in &doc.blocks {
        conn.execute(
            "INSERT OR REPLACE INTO blocks(id,doc_path,section_id,type,breadcrumb,raw,\
             hash,page_range,meta,ordinal,layer) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            params![
                b.id,
                doc.rel,
                b.section_id,
                b.kind,
                b.breadcrumb,
                b.body,
                b.hash,
                None::<String>,
                "{}",
                b.ordinal,
                doc.layer
            ],
        )?;
        // NOTE: no per-block `DELETE FROM blocks_fts WHERE id=?` here. The
        // doc-level delete above already removed every FTS row for this
        // doc_path, so this delete could never match -- but FTS5 has no index
        // on the id column, so each no-op delete scanned the whole FTS index.
        // Measured on the wiki layer: ~3 ms per block of pure waste.
        // store BOTH forms, mirroring the Python side
        conn.execute(
            "INSERT INTO blocks_fts(id,breadcrumb,raw) VALUES(?,?,?)",
            params![
                &b.id,
                format!("{} {}", b.breadcrumb, cjk_pad(&b.breadcrumb)),
                format!("{} {}", b.body, cjk_pad(&b.body))
            ],
        )?;
    }
    for sec in &doc.sections {
        // `layer` was hardcoded to 'raw' here, so every derived page's
        // sections were stored as raw-layer rows. Anything scoped to
        // layer='wiki' -- including the orphan check -- therefore skipped them
        // entirely, and the two shells disagreed about which rows were
        // source-of-truth.
        conn.execute(
            "INSERT OR REPLACE INTO sections(id,doc_path,parent_id,title,summary,\
             ordinal,layer) VALUES(?,?,?,?,?,?,?)",
            params![
                sec.id,
                doc.rel,
                sec.parent_id,
                sec.title,
                "" as &str,
                sec.ordinal,
                doc.layer
            ],
        )?;
    }

    // #2 -- a document's declared state applies to every block it produced.
    // Derived rather than persisted: absent or removed returns the card to
    // active, so repeated `index` runs converge instead of accumulating.
    match &doc.fm_declared_state {
        Some(st) if st != "active" => {
            for b in &doc.blocks {
                crate::state::set_block_state(conn, &b.id, st, "frontmatter state:")
                    .map_err(rusqlite::Error::InvalidParameterName)?;
            }
            // The §4.3 derived page is part of the card too: leaving it visible
            // would let a superseded document keep answering through its
            // summary after the source itself was hidden.
            let derived = format!("sources/{}", doc.rel);
            let mut dids: Vec<String> = Vec::new();
            {
                let mut q = conn.prepare("SELECT id FROM blocks WHERE doc_path=?")?;
                let m = q.query_map([&derived], |r| r.get::<_, String>(0))?;
                for r in m.flatten() {
                    dids.push(r);
                }
            }
            for id in dids {
                crate::state::set_block_state(conn, &id, st, "frontmatter state:")
                    .map_err(rusqlite::Error::InvalidParameterName)?;
            }
        }
        _ => {
            // No declared state: clear anything a previous run derived, so a
            // card edited back to active is not left superseded.
            conn.execute(
                "DELETE FROM block_state WHERE block_id IN \
                 (SELECT id FROM blocks WHERE doc_path=?) \
                 AND reason LIKE 'frontmatter state:%'",
                [&doc.rel],
            )?;
        }
    }
    Ok(doc.blocks.len())
}

/// One-step convenience for callers that do not run the two-phase index.
pub fn index_file(
    conn: &Connection,
    doc_path: &str,
    text: &str,
    layer: &str,
) -> rusqlite::Result<usize> {
    write_parsed_doc(conn, &parse_doc(doc_path, text, layer))
}

pub fn split_frontmatter(text: &str) -> (String, String) {
    if let Some(rest) = text.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            return (rest[..end].to_string(), rest[end + 4..].to_string());
        }
    }
    (String::new(), text.to_string())
}

/// Parse YAML frontmatter into the JSON object Python stores.
///
/// `documents.frontmatter` must hold JSON — `{"acl": ["finance-team"]}` — not
/// the raw YAML text. Every CONSUMER (acl::grants_for, cross-shell reads)
/// json.loads() this column, and Rust was writing raw YAML into it, so the
/// parse failed silently and every restricted document was pronounced PUBLIC.
/// That is how anonymous Studio exports leaked a restricted figure.
pub fn fm_to_json(fm: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for line in fm.lines() {
        let t = line.trim_end();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((k, v)) = t.split_once(':') else {
            continue;
        };
        let key = k.trim().trim_matches('"').to_string();
        let val = v.trim();
        let json_v = if val.starts_with('[') && val.ends_with(']') {
            // inline list: [finance-team] / [finance-team, hr]
            let items: Vec<serde_json::Value> = val[1..val.len() - 1]
                .split(',')
                .map(|x| x.trim().trim_matches('"').trim_matches('\''))
                .filter(|x| !x.is_empty())
                .map(|x| serde_json::Value::String(x.to_string()))
                .collect();
            serde_json::Value::Array(items)
        } else {
            serde_json::Value::String(val.trim_matches('"').trim_matches('\'').to_string())
        };
        map.insert(key, json_v);
    }
    serde_json::Value::Object(map)
}

/// Render frontmatter for storage — JSON, matching koiosbase's column format.
///
/// Python writes this column with `json.dumps()`, whose DEFAULT separators are
/// `", "` and `": "`. serde_json's `to_string` emits `","`/`":"` with no space,
/// so every stored frontmatter differed from Python's byte-for-byte — enough
/// for a consumer comparing the column as text (and enough to make a db-diff
/// between the shells look noisy).
pub fn fm_for_storage(fm: &str) -> String {
    json_python_dumps(&fm_to_json(fm))
}

/// Serialize with Python's `json.dumps` default separators (`, ` / `: `).
///
/// Key order follows the `Value`; that is insertion order because the crate
/// enables serde_json's `preserve_order` feature, matching Python dicts.
pub fn json_python_dumps(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => {
            serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
        }
        serde_json::Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(json_python_dumps)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        serde_json::Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, val)| format!(
                    "{}: {}",
                    serde_json::to_string(k).unwrap_or_else(|_| "\"\"".to_string()),
                    json_python_dumps(val)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The §8.2 state a document declares in its frontmatter, if any.
///
/// Without this, a card's only way to be superseded was `koios retract` on a
/// block id: there was no document-level source of state, so "this whole card
/// is obsolete" could not be expressed in the Markdown that is supposed to be
/// the source of truth. Unknown or absent means `active`, and removing the
/// key returns a card to active -- state is derived, never sticky.
pub fn fm_state(fm: &str) -> Option<String> {
    // `documents.frontmatter` stores JSON (`{"state": "superseded"}`) while
    // `index_file` still holds raw YAML (`state: superseded`). Accept both,
    // because which one a caller has depends on where in the pass it asks.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(fm) {
        if let Some(s) = v.get("state").and_then(|x| x.as_str()) {
            if crate::state::VALID_STATES.contains(&s) {
                return Some(s.to_string());
            }
        }
    }
    for line in fm.lines() {
        let t = line.trim_end();
        let Some(rest) = t.strip_prefix("state:") else {
            continue;
        };
        let v = rest
            .trim()
            .trim_matches('\'')
            .trim_matches('"')
            .trim_end_matches(',')
            .trim();
        if crate::state::VALID_STATES.contains(&v) {
            return Some(v.to_string());
        }
    }
    None
}

pub fn fm_title(fm: &str) -> Option<String> {
    for line in fm.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == "title" {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// `(rel_path, title, blocks)` for one raw document, as handed to `compile`.
pub type RawDoc = (String, String, Vec<pipeline::Ev>);
/// Own document -> its `(title, section_id)` list, in document order.
pub type SectionsByDoc = HashMap<String, Vec<(String, String)>>;

pub fn collect_raw_docs(
    conn: &rusqlite::Connection,
    vault: &Path,
) -> rusqlite::Result<(Vec<RawDoc>, SectionsByDoc)> {
    let mut docs: Vec<(String, String, Vec<pipeline::Ev>)> = Vec::new();
    let mut sections_by_doc: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut rels: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT DISTINCT doc_path FROM blocks WHERE layer='raw' ORDER BY doc_path")?;
        let m = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for r in m.flatten() {
            rels.push(r);
        }
    }
    for rel in rels {
        // Route through the ADAPTER, not by reading the file as text. The old
        // check skipped every non-`md` extension, so a PDF in raw/ contributed
        // blocks but never got its sources/ navigation page (§4.3) — Python
        // builds one from `_read_raw_docs`, which runs every raw doc through
        // `parse_any`. The blocks are already in the DB from the raw pass, so
        // only the title needs resolving here.
        let p = vault.join("raw").join(&rel);
        let title: String = if p.extension().and_then(|x| x.to_str()) == Some("md") {
            match fs::read_to_string(&p) {
                Ok(t) => {
                    let (fm, _) = split_frontmatter(&t);
                    fm_title(&fm).unwrap_or_else(|| rel.clone())
                }
                Err(_) => rel.clone(),
            }
        } else {
            // Non-Markdown: the title came out of the adapter's synthesized
            // frontmatter on the documents row.
            conn.query_row(
                "SELECT COALESCE(NULLIF(title,''),?) FROM documents WHERE path=?",
                params![rel, rel],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| rel.clone())
        };
        let mut blocks: Vec<pipeline::Ev> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id,raw,breadcrumb,doc_path FROM blocks WHERE doc_path=? AND layer='raw' \
                 ORDER BY ordinal",
            )?;
            let m = stmt.query_map(params![&rel], |r| {
                Ok(pipeline::Ev {
                    id: r.get(0)?,
                    raw: r.get::<_, String>(1).unwrap_or_default(),
                    breadcrumb: r.get::<_, String>(2).unwrap_or_default(),
                    doc_path: r.get::<_, String>(3).unwrap_or_default(),
                    channels: None,
                    state: None,
                })
            })?;
            for b in m.flatten() {
                blocks.push(b);
            }
        }
        let mut sections: Vec<(String, String)> = Vec::new();
        {
            let mut stmt =
                conn.prepare("SELECT title,id FROM sections WHERE doc_path=? ORDER BY ordinal")?;
            let m = stmt.query_map(params![&rel], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for t in m.flatten() {
                sections.push(t);
            }
        }
        sections_by_doc.insert(rel.clone(), sections);
        docs.push((rel, title, blocks));
    }
    Ok((docs, sections_by_doc))
}

pub fn walk_md(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if !matches!(
                        p.file_name().and_then(|n| n.to_str()),
                        Some(".index") | Some(".koios")
                    ) {
                        stack.push(p);
                    }
                } else if p.extension().and_then(|x| x.to_str()) == Some("md") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

pub fn walk_docs(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if !matches!(
                        p.file_name().and_then(|n| n.to_str()),
                        Some(".index") | Some(".koios")
                    ) {
                        stack.push(p);
                    }
                } else if ingest::is_supported(&p) {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

pub fn index_pdf(
    conn: &Connection,
    doc_path: &str,
    path: &std::path::Path,
    cfg: Option<&crate::llm::Config>,
) -> rusqlite::Result<usize> {
    // Two-tier (§5.1): the CPU tier takes every page it can read; only when a
    // page has no extractable text does the vision tier run, and only if one is
    // configured. No vision model means scanned pages stay invisible rather
    // than being fabricated — which is the honest default.
    let pages = match cfg.filter(|c| crate::llm::has_vision(c)) {
        // `f` must own the config: `extract_pages` takes `&VlmFn`, and a
        // closure borrowing a function parameter cannot outlive the call.
        Some(c) => {
            let owned = c.clone();
            let f = move |_p: &str, png: &[u8]| -> Result<String, String> {
                crate::llm::vision(&owned, png)
            };
            pdf::extract_pages(path, Some(&f))
        }
        None => pdf::extract_pages_cpu(path),
    };
    let scanned = pdf::looks_scanned(&pages, 40);
    let fm = pdf::pdf_frontmatter(path, scanned);
    // `pdf_frontmatter` now emits the finished JSON object (Python builds it as
    // a dict and json.dumps() it), so it must NOT go through fm_for_storage —
    // that would re-encode the whole object as one string value.
    let title = serde_json::from_str::<serde_json::Value>(&fm)
        .ok()
        .and_then(|v| v.get("title").and_then(|t| t.as_str()).map(str::to_string))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| doc_path.to_string());
    conn.execute(
        "INSERT OR REPLACE INTO documents(path,title,frontmatter,hash) VALUES(?,?,?,?)",
        params![doc_path, title, fm, ""],
    )?;
    let mut count = 0usize;
    let mut ordinal = 0usize;
    for (i, text) in pages.iter().enumerate() {
        let page_no = i + 1;
        let sid = format!("{doc_path}#p{page_no}");
        conn.execute(
            "INSERT OR REPLACE INTO sections(id,doc_path,parent_id,title,summary,ordinal,layer)\
             VALUES(?,?,NULL,?,?,?,'raw')",
            params![
                sid,
                doc_path,
                format!("p.{page_no}"),
                "" as &str,
                page_no as i64
            ],
        )?;
        for b in pdf::page_to_blocks(doc_path, page_no, text, ordinal) {
            ordinal = ordinal.max(
                b.id.rsplit('/')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(ordinal),
            );
            // Python stores json.dumps(b.page_range) / json.dumps(b.meta), which
            // use ", " / ": " separators — the columns must match byte-for-byte.
            let page_range = format!("[{page_no}, {page_no}]");
            let meta = r#"{"source_format": "pdf"}"#.to_string();
            let kind = if b.raw.contains('|') {
                "table"
            } else {
                "paragraph"
            };
            conn.execute(
                "INSERT OR REPLACE INTO blocks(id,doc_path,section_id,type,breadcrumb,raw,hash,\
                 page_range,meta,ordinal,layer) VALUES(?,?,?,?,?,?,?,?,?,?,'raw')",
                params![
                    b.id,
                    doc_path,
                    sid,
                    kind,
                    b.breadcrumb,
                    b.raw,
                    // Same stable hash as the Markdown path — breadcrumb + "\n"
                    // + raw, whitespace-normalized. Without it every PDF block
                    // stored "" and incremental re-indexing could not tell an
                    // edited PDF from an unchanged one.
                    content_hash(&format!("{}\n{}", b.breadcrumb, b.raw)),
                    page_range,
                    meta,
                    ordinal as i64
                ],
            )?;
            conn.execute("DELETE FROM blocks_fts WHERE id=?", params![&b.id])?;
            conn.execute(
                "INSERT INTO blocks_fts(id,breadcrumb,raw) VALUES(?,?,?)",
                params![
                    &b.id,
                    format!("{} {}", b.breadcrumb, cjk_pad(&b.breadcrumb)),
                    format!("{} {}", b.raw, cjk_pad(&b.raw))
                ],
            )?;
            count += 1;
        }
    }
    Ok(count)
}

/// §4.4 — the maintenance contract written INTO the vault, so it travels with
/// the knowledge rather than living only in this repo's docs.
///
/// Lives in the lib (not main.rs) so integration tests can assert the vault a
/// real `koios init` produces — `cmd_index` lives here for the same reason.
pub fn init_vault(path: &Path) -> std::io::Result<()> {
    for d in [
        "raw",
        "wiki/sources",
        "wiki/entities",
        "wiki/concepts",
        "wiki/synthesis",
        "wiki/answers",
        ".index",
    ] {
        fs::create_dir_all(path.join(d))?;
    }
    let agents = path.join("AGENTS.md");
    if !agents.exists() {
        // Byte-identical to ingest/pipeline.py cmd_init() — this file is the
        // §4.4 maintenance contract written INTO the vault, so a short version
        // silently drops three rules the vault is supposed to carry.
        fs::write(
            agents,
            "# AGENTS.md\n\n\
             KoiosBase maintenance contract. `raw/` and `wiki/` are Markdown and are\n\
             the source of truth; `.index/` is derived and rebuildable.\n\n\
             - Every factual assertion in `wiki/` must carry a `[[wikilink]]` to raw.\n\
             - New compiled pages start at `confidence: draft`.\n\
             - Promotion requires cross-family verification or human confirmation,\n  \
             never citation counts (§5.4).\n\
             - Rebuild at any time: `koios index <vault>`\n",
        )?;
    }
    // Python also marks the derived dir as ignored; without it a built vault
    // looks dirty to git.
    fs::write(path.join(".index").join(".gitignore"), "*\n")?;
    Ok(())
}

/// Mutual exclusion for `koios index`, held for the duration of one run.
///
/// An exclusive create (`O_EXCL`) is the whole mechanism: it is atomic in the
/// kernel, so two processes racing cannot both win. Stale locks from a crash
/// are recovered by checking whether the PID recorded inside is still alive --
/// without that, one interrupted run would wedge every future index forever.
pub struct IndexLock {
    path: PathBuf,
}

impl IndexLock {
    fn file(vault: &Path) -> PathBuf {
        vault.join(".index").join("index.lock")
    }

    pub fn acquire(vault: &Path) -> Result<Self, std::io::Error> {
        let dir = vault.join(".index");
        std::fs::create_dir_all(&dir)?;
        let path = Self::file(vault);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                let _ = writeln!(f, "{}", std::process::id());
                let _ = f.flush();
                Ok(IndexLock { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Is the owner still alive? A lock file left by a killed
                // process must not block indexing forever.
                if Self::holder_is_dead(&path) {
                    let _ = std::fs::remove_file(&path);
                    return Self::acquire(vault);
                }
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("lock held: {}", path.display()),
                ))
            }
            Err(e) => Err(e),
        }
    }

    /// True when the recorded PID is absent, unparsable, or no longer running.
    fn holder_is_dead(path: &Path) -> bool {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return true,
        };
        let pid: i32 = match text.trim().parse() {
            Ok(p) => p,
            Err(_) => return true,
        };
        if pid <= 0 {
            return true;
        }
        // Signal 0 is the existence probe: it succeeds iff the process lives.
        #[cfg(unix)]
        {
            unsafe { libc::kill(pid, 0) != 0 }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn cmd_index(path: &Path) -> rusqlite::Result<()> {
    // §15 — one writer at a time.
    //
    // `koios index` rewrites the whole derived index inside a transaction, and
    // nothing stopped a second process (a long-lived MCP server, a cron job,
    // two terminals) from starting its own concurrently. The result was not
    // obviously broken, which is worse: two successful-looking runs against
    // overlapping state. Callers had to invent their own file lock.
    let _guard = match IndexLock::acquire(path) {
        Ok(g) => g,
        Err(e) => {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "another `koios index` is already running on this vault ({e}). \
                 Wait for it, or remove {} if it is stale.",
                path.join(".index").join("index.lock").display()
            )));
        }
    };
    let conn = connect(path)?;
    // Wrap the whole ingest in ONE transaction. Without it every INSERT is its
    // own implicit transaction and fsyncs, which dominated the runtime.
    let tx = conn.unchecked_transaction()?;
    // Snapshot BEFORE ingest: afterwards the index already holds the new
    // content, so there is nothing left to diff it against.
    let before_fp = raw_doc_fingerprints(&conn);
    // links are DERIVED from Markdown (§5.2), so a rebuild must start from an
    // empty edge set — otherwise deleted wikilinks keep polluting the graph.
    conn.execute("DELETE FROM links", [])?;
    let mut total = 0usize;
    let mut seen_links: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();
    // Two-pass, mirroring `build_index`: raw first, THEN regenerate sources/,
    // then the wiki walk sees what was just written. The naive single pass
    // failed on a cold vault — generate-before-walk could not see any raw
    // blocks yet, so run 1 counted only raw and run 2 counted raw+derived,
    // making the command look non-idempotent on its very first invocation.
    // Read the vault's config once, not per file: the vision tier would
    // otherwise re-parse koios.toml for every scanned page.
    let cfg = crate::llm::load(path).unwrap_or_default();
    let mut indexed_raw = false;
    for (layer, dir_name) in [("raw", "raw"), ("wiki", "wiki")] {
        if indexed_raw {
            let (docs, sections_by_doc) = collect_raw_docs(&conn, path)?;
            if !docs.is_empty() {
                let stamp = state::now_date();
                compile::write_source_pages(path, &docs, &stamp, &sections_by_doc).ok();
            }
        }
        let base = path.join(dir_name);
        if !base.exists() {
            continue;
        }
        // Phase A — parallel parse. Reading and blockifying Markdown is pure
        // CPU work with no DB access, so it runs across every core (rayon's
        // pool sizes itself to available_parallelism; deliberately no config
        // knob). Phase B below applies the rows serially, so SQLite still
        // sees exactly one writer. Determinism: par_iter preserves input
        // order, so writes land in the same order the serial loop used.
        let mut md_files: Vec<(String, std::path::PathBuf)> = Vec::new();
        let mut pdf_files: Vec<(String, std::path::PathBuf)> = Vec::new();
        for entry in walk_docs(&base) {
            let rel = entry
                .strip_prefix(&base)
                .unwrap()
                .to_string_lossy()
                .to_string();
            if entry.extension().and_then(|x| x.to_str()) == Some("pdf") {
                pdf_files.push((rel, entry));
            } else {
                md_files.push((rel, entry));
            }
        }
        let parsed: Vec<ParsedDoc> = {
            use rayon::prelude::*;
            md_files
                .par_iter()
                .map(|(rel, p)| {
                    let text = fs::read_to_string(p).unwrap_or_default();
                    parse_doc(rel, &text, layer)
                })
                .collect()
        };
        // Phase B — serial writes. Derived pages ARE indexed (see
        // `ingest::is_derived_page`): content hashes make ingest idempotent,
        // and excluding them starved channel ① (§6.1, wiki-first).
        for doc in &parsed {
            total += write_parsed_doc(&conn, doc)?;
            // Graph edges derive purely from Markdown (§5.2); the edge rows
            // come straight from the parsed blocks instead of re-SELECTing
            // them one query per file as before.
            ingest::add_wikilink_edges(&conn, &doc.evs, &mut seen_links)?;
        }
        for (rel, entry) in pdf_files {
            total += index_pdf(&conn, &rel, &entry, Some(&cfg))?;
        }
        // Structural edges are built once the whole corpus is indexed, after
        // the per-document wikilink pass. Without them the graph is empty on
        // card-style corpora and channel ③ silently degenerates into a copy of
        // BM25 -- measured: links had zero rows, and `graph` returned exactly
        // what `hybrid` returned.
        ingest::add_structural_edges(&conn)?;
        // Links written mid-pass cannot see documents indexed after them, so a
        // link to a not-yet-read card (or one named by its heading) has no
        // target at creation time. Repair now that every card exists.
        ingest::resolve_pending_links(&conn)?;
        if layer == "raw" {
            indexed_raw = true;
        }
    }
    // #2 — declared document state, applied after everything is indexed so it
    // reaches the §4.3 derived pages too (they are produced later in the pass).
    // Derived from Markdown each run: removing the key returns the card to
    // active, so repeated `index` converges instead of accumulating.
    apply_declared_states(&conn)?;
    // Human block-level decisions are also Markdown facts. Apply them after the
    // document-level declaration so the more specific decision wins, and replay
    // their citation cascade to rebuild page_state after a cold .index rebuild.
    crate::state::apply_markdown_states(&conn, path)
        .map_err(rusqlite::Error::InvalidParameterName)?;
    // Backfill navigational summaries: a section's first block becomes its
    // summary (§4.1). Done after ingest because the body arrives AFTER the
    // heading, so it is not available when the heading line is seen.
    //
    // ingest/pipeline.py computes `own[0].replace("\n", " ")[:160]` — newlines
    // collapse to spaces and the result is CHARACTER-truncated (SQLite's
    // substr() is character-based on TEXT, matching Python slicing). The raw
    // multiline form leaked into the column, which is what a §4.1 drill-down
    // renders, so the same navigation looked different on the two shells.
    conn.execute(
        "UPDATE sections SET summary = COALESCE((SELECT \
         substr(replace(raw, char(10), ' '), 1, 160) FROM blocks \
         WHERE blocks.section_id = sections.id ORDER BY ordinal LIMIT 1), '')",
        [],
    )?;
    // Deletion reclaim. Re-index refreshes whatever is present but never
    // removes anything: a deleted raw document left its blocks, sections, rows
    // in `documents` and its generated wiki/sources page behind, all still answering
    // queries, and the only remedy was throwing away .index/ whole. The corpus
    // could only grow. Deletes happen inside the same transaction as the rest
    // of ingest so a failed run cannot leave a half-purged document.
    if !before_fp.is_empty() {
        let gone = missing_on_disk(path, before_fp.keys());
        for doc in &gone {
            let n = purge_document(&conn, doc)?;
            if n > 0 {
                eprintln!("removed {doc} ({n} blocks) — source no longer present");
            }
            // The derived page is indexed in its own right (§4.3), so its
            // blocks survive the parent's removal unless purged too. Leaving
            // them meant a deleted document kept answering queries through
            // channel ①, which prefers wiki rows.
            let derived = format!("sources/{doc}");
            let m = purge_document(&conn, &derived)?;
            if m > 0 {
                eprintln!("removed {derived} ({m} blocks) — derived page of a deleted source");
            }
        }
        compile::retire_source_pages(path, &gone).ok();
    }
    tx.commit()?;
    // §8.3 write side: an edited raw document invalidates the synthesis pages
    // built from it. Nothing wired this up, so `page_state` stayed empty no
    // matter how many times the vault was edited — which is why the read side
    // (apply_disposition, the （待更新） marker) had nothing to act on.
    // Only documents whose block hashes actually moved are reported; re-indexing
    // an untouched vault must invalidate nothing. A brand-new vault in
    // particular has no fingerprint history, and flagging every synthesis page
    // on the first `index` would be noise, not signal.
    let after_fp = raw_doc_fingerprints(&conn);
    let first_run = before_fp.is_empty();
    let changed = if first_run {
        Vec::new()
    } else {
        changed_raw_docs(&before_fp, &after_fp)
    };
    if !crate::embed::allow_lexical() {
        crate::embed::require(path).map_err(rusqlite::Error::InvalidParameterName)?;
    }
    // The opt-out has to gate the BUILD too, not just the check: `is_ready`
    // only asks whether the files are present, so `--allow-no-embedding` was
    // still encoding every block. Measured: 0.17s -> 79s on 600 blocks with
    // the flag supposedly off.
    if !crate::embed::allow_lexical() && crate::embed::is_ready(path) {
        crate::embed::build_vectors(path, &conn)?;
    }
    let flagged = if changed.is_empty() {
        Vec::new()
    } else {
        let topics = compile::syntheses_citing(path, &changed);
        if topics.is_empty() {
            Vec::new()
        } else {
            compile::mark_syntheses_stale(&conn, path, Some(&topics)).unwrap_or_default()
        }
    };
    println!("indexed {total} blocks from {}", path.display());
    if !flagged.is_empty() {
        println!(
            "stale: {} synthesis page(s) — run `koios compile` to refresh",
            flagged.len()
        );
    }
    Ok(())
}

/// Per-raw-document fingerprint: one sha256 over every block's own hash.
///
/// `documents.hash` is written empty by `upsert_document`, and comparing a
/// whole-file digest against nothing is how the first attempt at this failed.
/// The per-block hashes are authoritative, so fold those instead — ordering by
/// `ordinal` so a section that merely moved does not read as an edit.
fn raw_doc_fingerprints(conn: &Connection) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT doc_path, group_concat(hash, '|') FROM \
         (SELECT doc_path, hash FROM blocks WHERE layer='raw' ORDER BY doc_path, ordinal) \
         GROUP BY doc_path",
    ) else {
        return out;
    };
    let Ok(m) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1).unwrap_or_default(),
        ))
    }) else {
        return out;
    };
    for r in m.flatten() {
        out.insert(r.0, r.1);
    }
    out
}

/// Raw docs whose fingerprint differs between two snapshots — i.e. the ones an
/// ingest actually changed. Comparing before and after is the only honest way:
/// afterwards the index already holds the new content, so there is nothing left
/// to diff it against.
fn changed_raw_docs(
    before: &std::collections::HashMap<String, String>,
    after: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let keys: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    keys.into_iter()
        .filter(|k| before.get(*k) != after.get(*k))
        .cloned()
        .collect()
}

/// Of these raw document paths, the ones with no file on disk any more.
///
/// Diffing successive fingerprints did not work, and the reason is worth
/// keeping: `wiki/sources/x.md` is itself indexed, and during the same run it
/// regenerates a page whose blocks quote the raw text. So `after` still listed
/// x.md even with raw/x.md deleted — nothing looked missing. The filesystem is
/// the only authority here; an in-memory diff of derived state cannot tell
/// "still present" from "rebuilt from a stale copy a moment ago".
fn missing_on_disk<'a>(vault: &Path, docs: impl Iterator<Item = &'a String>) -> Vec<String> {
    let raw_dir = vault.join("raw");
    let mut out: Vec<String> = docs
        .filter(|d| !raw_dir.join(d).is_file())
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Drop every trace of a document that no longer exists on disk.
///
/// Order matters: `blocks_fts` and `links` reference block ids, so they are
/// cleared before the `blocks` row goes; `sections` after, since it parents
/// them. The §8.2 state tables are cleaned too, otherwise `block_state` keeps
/// a retracted verdict attached to a block id that may reappear later.
fn purge_document(conn: &Connection, doc: &str) -> rusqlite::Result<usize> {
    let mut removed = 0usize;
    let mut ids: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id FROM blocks WHERE doc_path=?")?;
        let m = stmt.query_map([doc], |r| r.get::<_, String>(0))?;
        for r in m.flatten() {
            ids.push(r);
        }
    }
    for id in &ids {
        conn.execute("DELETE FROM blocks_fts WHERE id=?", [id])?;
        conn.execute("DELETE FROM links WHERE src=? OR dst=?", [id, id])?;
        conn.execute("DELETE FROM block_state WHERE block_id=?", [id])?;
        removed += conn.execute("DELETE FROM blocks WHERE id=?", [id])?;
    }
    conn.execute("DELETE FROM sections WHERE doc_path=?", [doc])?;
    conn.execute("DELETE FROM page_state WHERE page_path=?", [doc])?;
    conn.execute("DELETE FROM documents WHERE path=?", [doc])?;
    Ok(removed)
}
