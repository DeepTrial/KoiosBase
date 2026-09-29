//! KoiosBase Rust CLI — spike (init / index / search).
//!
//! Goal: verify a compiled shell can drive the SAME vault format as the Python
//! implementation. Two things must hold, or the spike fails:
//!   1. the on-disk .index/tree.db written here is readable by the Python side
//!      (single source of truth = Markdown, index is derived — P1)
//!   2. Chinese BM25 search behaves like the Python side: FTS5's unicode61
//!      tokenizer treats a contiguous CJK run as ONE token, so we store BOTH the
//!      original text and a per-character padded form, exactly as Python does.

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use rusqlite::{params, Connection};

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
"#;

#[derive(Parser)]
#[command(name = "koios", version, about = "KoiosBase (Rust shell spike)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// scaffold a vault
    Init { path: Option<PathBuf> },
    /// build the derived index from raw/ + wiki/
    Index { path: Option<PathBuf> },
    /// BM25 search over blocks
    Search {
        /// query text
        query: Vec<String>,
        /// vault directory
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
        #[arg(short = 'k', long, default_value_t = 8)]
        top: usize,
    },
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// Insert spaces between CJK characters — mirrors Python's cjk_pad().
fn cjk_pad(text: &str) -> String {
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

fn connect(vault: &Path) -> rusqlite::Result<Connection> {
    let dir = vault.join(".index");
    fs::create_dir_all(&dir).ok();
    let conn = Connection::open(dir.join("tree.db"))?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

fn cmd_init(path: &Path) -> std::io::Result<()> {
    for d in ["raw", "wiki/sources", "wiki/entities", "wiki/concepts",
              "wiki/synthesis", "wiki/answers", ".index"] {
        fs::create_dir_all(path.join(d))?;
    }
    let agents = path.join("AGENTS.md");
    if !agents.exists() {
        fs::write(agents, "# AGENTS.md\n\nKoiosBase maintenance contract.\n")?;
    }
    println!("initialized KoiosBase vault at {}", path.display());
    Ok(())
}

/// Minimal Markdown splitter: headings become sections, blank-line separated
/// paragraphs become blocks. Tables/code are kept whole (P2).
fn index_file(conn: &Connection, doc_path: &str, text: &str) -> rusqlite::Result<usize> {
    let (fm, body) = split_frontmatter(text);
    let title = fm_title(&fm).unwrap_or_else(|| doc_path.to_string());
    conn.execute(
        "INSERT OR REPLACE INTO documents(path,title,frontmatter,hash) VALUES(?,?,?,?)",
        params![doc_path, title, fm, ""],
    )?;

    let mut breadcrumb = title.clone();
    let mut ordinal = 0usize;
    let mut count = 0usize;

    // Parse line-by-line: headings and body may be separated by a SINGLE
    // newline, so paragraph-splitting on "\n\n" would glue a heading to its body
    // and the whole thing would be swallowed as a heading (0 blocks indexed).
    let mut pending: Vec<String> = Vec::new();

    let flush = |pending: &mut Vec<String>, breadcrumb: &mut String,
                 ordinal: &mut usize, count: &mut usize,
                 conn: &Connection, doc_path: &str| -> rusqlite::Result<()> {
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
        let id = format!("{}#{}", doc_path, *ordinal);
        let kind = if body.contains('|') { "table" } else { "paragraph" };
        conn.execute(
            "INSERT OR REPLACE INTO blocks(id,doc_path,section_id,type,breadcrumb,raw,\
             hash,page_range,meta,ordinal,layer) VALUES(?,?,?,?,?,?,?,?,?,?,'raw')",
            params![id, doc_path, doc_path, kind, *breadcrumb, body, "", None::<String>,
                    "{}", *ordinal],
        )?;
        conn.execute("DELETE FROM blocks_fts WHERE id=?", params![&id])?;
        // store BOTH forms, mirroring the Python side
        conn.execute(
            "INSERT INTO blocks_fts(id,breadcrumb,raw) VALUES(?,?,?)",
            params![&id, format!("{} {}", *breadcrumb, cjk_pad(breadcrumb)),
                    format!("{} {}", body, cjk_pad(&body))],
        )?;
        Ok(())
    };

    for line in body.lines() {
        let s = line.trim();
        if s.is_empty() {
            flush(&mut pending, &mut breadcrumb, &mut ordinal, &mut count, conn, doc_path)?;
            continue;
        }
        if let Some(heading) = s.strip_prefix('#') {
            flush(&mut pending, &mut breadcrumb, &mut ordinal, &mut count, conn, doc_path)?;
            breadcrumb = heading.trim_start_matches('#').trim().to_string();
            continue;
        }
        pending.push(line.to_string());
    }
    flush(&mut pending, &mut breadcrumb, &mut ordinal, &mut count, conn, doc_path)?;
    Ok(count)
}

fn split_frontmatter(text: &str) -> (String, String) {
    if let Some(rest) = text.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            return (rest[..end].to_string(), rest[end + 4..].to_string());
        }
    }
    (String::new(), text.to_string())
}

fn fm_title(fm: &str) -> Option<String> {
    for line in fm.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == "title" {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn cmd_index(path: &Path) -> rusqlite::Result<()> {
    let conn = connect(path)?;
    // Wrap the whole ingest in ONE transaction. Without it every INSERT is its
    // own implicit transaction and fsyncs, which dominated the runtime.
    let tx = conn.unchecked_transaction()?;
    let mut total = 0usize;
    for (layer, dir_name) in [("raw", "raw"), ("wiki", "wiki")] {
        let base = path.join(dir_name);
        if !base.exists() {
            continue;
        }
        for entry in walk_md(&base) {
            let rel = entry.strip_prefix(&base).unwrap().to_string_lossy().to_string();
            // Derived pages (sources/, entities/) are regenerated by the
            // compiler and must NOT be indexed, or every rebuild feeds the
            // compiler's own output back in and the block count grows forever
            // (this exact bug bit the Python side in v0.2 and v0.3).
            if layer == "wiki" && (rel.starts_with("sources/") || rel.starts_with("entities/")) {
                continue;
            }
            let text = fs::read_to_string(&entry).unwrap_or_default();
            total += index_file(&conn, &rel, &text)?;
        }
    }
    tx.commit()?;
    println!("indexed {total} blocks from {}", path.display());
    Ok(())
}

fn walk_md(base: &Path) -> Vec<PathBuf> {
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

fn cmd_search(path: &Path, query: &str, top: usize) -> rusqlite::Result<()> {
    let conn = connect(path)?;
    let terms: Vec<String> = query
        .split_whitespace()
        .map(cjk_pad)
        .collect();
    if terms.is_empty() {
        println!("(no hits)");
        return Ok(());
    }
    let quoted: Vec<String> = terms.iter().map(|t| format!("\"{}\"", t.trim())).collect();
    // AND first (precision), then OR (recall) — same ladder as Python.
    for expr in [quoted.join(" AND "), quoted.join(" OR ")] {
        let sql = "SELECT id, bm25(blocks_fts) AS score FROM blocks_fts \
                   WHERE blocks_fts MATCH ? ORDER BY score LIMIT ?";
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rows: Vec<(String, f64)> = stmt
            .query_map(params![expr, top as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        if rows.is_empty() {
            continue;
        }
        for (id, score) in rows {
            let raw: String = conn
                .query_row("SELECT raw FROM blocks WHERE id=?", params![id], |r| r.get(0))
                .unwrap_or_default();
            let snippet: String = raw.chars().take(110).collect();
            println!("[{score:.4}] {id}\n    {snippet}");
        }
        return Ok(());
    }
    println!("(no hits)");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { path } => cmd_init(&path.unwrap_or_else(|| PathBuf::from(".")))?,
        Cmd::Index { path } => cmd_index(&path.unwrap_or_else(|| PathBuf::from(".")))?,
        Cmd::Search { query, path, top } => cmd_search(&path, &query.join(" "), top)?,
    }
    Ok(())
}
