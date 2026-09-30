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
pub mod compile;
pub mod ingest;
pub mod lint;
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
CREATE INDEX IF NOT EXISTS idx_links_src ON links(src);
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
    Ok(conn)
}

use rusqlite::params;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
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

pub fn index_file(
    conn: &Connection,
    doc_path: &str,
    text: &str,
    layer: &str,
) -> rusqlite::Result<usize> {
    let (fm, body) = split_frontmatter(text);
    let title = fm_title(&fm).unwrap_or_else(|| doc_path.to_string());
    conn.execute(
        "INSERT OR REPLACE INTO documents(path,title,frontmatter,hash) VALUES(?,?,?,?)",
        params![doc_path, title, fm_for_storage(&fm), ""],
    )?;

    let title = fm_title(&fm).unwrap_or_else(|| doc_path.to_string());
    let mut ordinal = 0usize;
    let mut count = 0usize;
    // Heading stack mirrors koiosbase/parsers/markdown.py: (level, path_titles).
    // Block ids must be byte-identical to Python's
    //   f"{doc_path}#{'/'.join(section_titles)}/{ordinal}"
    // or the two shells write different trees into the same schema and eval
    // results cannot be compared (P1: index is derived, Markdown is truth).
    let base_title = title.clone();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut section_no = 0i64;

    let flush = |pending: &mut Vec<String>,
                 sec_path: &str,
                 breadcrumb: &str,
                 ordinal: &mut usize,
                 count: &mut usize,
                 conn: &Connection,
                 doc_path: &str|
     -> rusqlite::Result<()> {
        let body: String = pending
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        pending.clear();
        if body.is_empty() {
            return Ok(());
        }
        *ordinal += 1;
        *count += 1;
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
            breadcrumb.to_string()
        } else {
            format!(
                "{} > {}",
                breadcrumb,
                sec_path.split('/').collect::<Vec<_>>().join(" > ")
            )
        };
        let kind = if body.contains('|') {
            "table"
        } else {
            "paragraph"
        };
        conn.execute(
            "INSERT OR REPLACE INTO blocks(id,doc_path,section_id,type,breadcrumb,raw,\
             hash,page_range,meta,ordinal,layer) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                doc_path,
                sec_id,
                kind,
                crumb,
                body,
                "",
                None::<String>,
                "{}",
                *ordinal,
                layer
            ],
        )?;
        conn.execute("DELETE FROM blocks_fts WHERE id=?", params![&id])?;
        // store BOTH forms, mirroring the Python side
        conn.execute(
            "INSERT INTO blocks_fts(id,breadcrumb,raw) VALUES(?,?,?)",
            params![
                &id,
                format!("{} {}", crumb, cjk_pad(&crumb)),
                format!("{} {}", body, cjk_pad(&body))
            ],
        )?;
        Ok(())
    };

    // sec_path: current "/"-joined heading titles; "" means no heading yet.
    let mut sec_path = String::new();
    for line in body.lines() {
        let s = line.trim();
        if s.is_empty() {
            flush(
                &mut pending,
                &sec_path,
                &base_title,
                &mut ordinal,
                &mut count,
                conn,
                doc_path,
            )?;
            continue;
        }
        if let Some(heading) = s.strip_prefix('#') {
            flush(
                &mut pending,
                &sec_path,
                &base_title,
                &mut ordinal,
                &mut count,
                conn,
                doc_path,
            )?;
            let hashes = s.chars().take_while(|c| *c == '#').count();
            let title = heading.trim_start_matches('#').trim().to_string();
            let level = hashes.min(6);
            while stack.last().map(|(l, _)| *l >= level).unwrap_or(false) {
                stack.pop();
            }
            stack.push((level, title.clone()));
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
            conn.execute(
                "INSERT OR REPLACE INTO sections(id,doc_path,parent_id,title,summary,\
                 ordinal,layer) VALUES(?,?,?,?,?,?,'raw')",
                params![sid, doc_path, parent_id, title, "" as &str, section_no],
            )?;
            continue;
        }
        pending.push(line.to_string());
    }
    flush(
        &mut pending,
        &sec_path,
        &base_title,
        &mut ordinal,
        &mut count,
        conn,
        doc_path,
    )?;
    Ok(count)
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
pub fn fm_for_storage(fm: &str) -> String {
    serde_json::to_string(&fm_to_json(fm)).unwrap_or_else(|_| "{}".to_string())
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

pub fn collect_raw_docs(
    conn: &rusqlite::Connection,
    vault: &Path,
) -> rusqlite::Result<(
    Vec<(String, String, Vec<pipeline::Ev>)>,
    HashMap<String, Vec<(String, String)>>,
)> {
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
        // Route through the adapter: a PDF in raw/ parsed as Markdown would
        // produce garbage, so non-Markdown sources are skipped (§5.1).
        let p = vault.join("raw").join(&rel);
        if p.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let text = match fs::read_to_string(&p) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let (fm, _body) = split_frontmatter(&text);
        let title = fm_title(&fm).unwrap_or_else(|| rel.clone());
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
                    if p.file_name().and_then(|n| n.to_str()) != Some(".index") {
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
                    if p.file_name().and_then(|n| n.to_str()) != Some(".index") {
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
) -> rusqlite::Result<usize> {
    let pages = pdf::extract_pages_cpu(path);
    let scanned = pdf::looks_scanned(&pages, 40);
    let fm = pdf::pdf_frontmatter(path, scanned);
    let title = fm_title(&fm).unwrap_or_else(|| doc_path.to_string());
    conn.execute(
        "INSERT OR REPLACE INTO documents(path,title,frontmatter,hash) VALUES(?,?,?,?)",
        params![doc_path, title, fm_for_storage(&fm), ""],
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
                    "",
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

pub fn cmd_index(path: &Path) -> rusqlite::Result<()> {
    let conn = connect(path)?;
    // Wrap the whole ingest in ONE transaction. Without it every INSERT is its
    // own implicit transaction and fsyncs, which dominated the runtime.
    let tx = conn.unchecked_transaction()?;
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
        for entry in walk_docs(&base) {
            let rel = entry
                .strip_prefix(&base)
                .unwrap()
                .to_string_lossy()
                .to_string();
            // Derived pages ARE indexed. See `ingest::is_derived_page` for the
            // full reasoning; in short: content hashes make ingest idempotent,
            // so counting a regenerated page cannot grow the block set across
            // rebuilds (verified: three consecutive rebuilds are stable), while
            // excluding them starved channel ① (§6.1, wiki-first) of the wiki
            // rows it is supposed to prefer. Self-feeding is prevented on the
            // compile side, whose input set is pinned to layer='raw'.
            //
            // The previous `continue` here was carried over from a Python bug
            // that is long fixed, and its stated reason — "block count grows
            // forever" — does not hold for hash-based ingest.
            let n = if entry.extension().and_then(|x| x.to_str()) == Some("pdf") {
                index_pdf(&conn, &rel, &entry)?
            } else {
                let text = fs::read_to_string(&entry).unwrap_or_default();
                index_file(&conn, &rel, &text, layer)?
            };
            total += n;
            // Graph edges derive purely from Markdown (§5.2). Incremental
            // rebuilds would otherwise leave dangling edges behind, so the
            // whole table is cleared above and rebuilt here.
            let mut evs: Vec<pipeline::Ev> = Vec::new();
            {
                let mut stmt = conn.prepare(
                    "SELECT id,raw,breadcrumb,doc_path FROM blocks \
                     WHERE doc_path=? AND layer=? ORDER BY ordinal",
                )?;
                let m = stmt.query_map(params![rel, layer], |r| {
                    Ok(pipeline::Ev {
                        id: r.get(0)?,
                        raw: r.get::<_, String>(1).unwrap_or_default(),
                        breadcrumb: r.get::<_, String>(2).unwrap_or_default(),
                        doc_path: r.get::<_, String>(3).unwrap_or_default(),
                    })
                })?;
                for e in m.flatten() {
                    evs.push(e);
                }
            }
            ingest::add_wikilink_edges(&conn, &evs, &mut seen_links)?;
        }
        if layer == "raw" {
            indexed_raw = true;
        }
    }
    // Backfill navigational summaries: a section's first block becomes its
    // summary (§4.1). Done after ingest because the body arrives AFTER the
    // heading, so it is not available when the heading line is seen.
    let mut upd = conn.prepare(
        "UPDATE sections SET summary = COALESCE((SELECT raw FROM blocks \
         WHERE blocks.section_id = sections.id ORDER BY ordinal LIMIT 1), '')",
    )?;
    upd.execute([])?;
    drop(upd);
    tx.commit()?;
    println!("indexed {total} blocks from {}", path.display());
    Ok(())
}
