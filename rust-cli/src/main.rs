//! KoiosBase Rust CLI — spike (init / index / search).
//!
//! Goal: verify a compiled shell can drive the SAME vault format as the Python
//! implementation. Two things must hold, or the spike fails:
//!   1. the on-disk .index/tree.db written here is readable by the Python side
//!      (single source of truth = Markdown, index is derived — P1)
//!   2. Chinese BM25 search behaves like the Python side: FTS5's unicode61
//!      tokenizer treats a contiguous CJK run as ONE token, so we store BOTH the
//!      original text and a per-character padded form, exactly as Python does.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

mod acl;
mod compile;
mod ingest;
mod lint;
mod mcp;
mod pipeline;
mod retrieval;
mod state;
mod studio;

use clap::{Parser, Subcommand};
use rusqlite::{params, Connection};

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
    /// BM25 + PPR hybrid search (channel ②+③)
    Search {
        /// query text
        query: Vec<String>,
        /// vault directory
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
        #[arg(short = 'k', long, default_value_t = 8)]
        top: usize,
    },
    /// channel ① tree navigation
    Tree {
        query: Vec<String>,
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
    },
    /// channel ④ full-corpus (empty when over the threshold)
    Full {
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 200_000)]
        max_chars: usize,
    },
    /// full query with Grader verdict (§6.2)
    Query {
        query: Vec<String>,
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
        #[arg(short = 'k', long, default_value_t = 8)]
        top: usize,
    },
    /// eval harness metrics (§10.2)
    Eval {
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
    },
    /// L1 programmatic lint checks (§9.3, §10.3)
    Lint { path: Option<PathBuf> },
    /// cross-judge one claim against evidence (§10.3 L2)
    CheckClaim { claim: String, evidence: String },
    /// retract a block and cascade staleness to citing pages (§8.3)
    Retract {
        block_id: String,
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// compile entities/ + sources/ from raw (§5.3)
    Compile {
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
    },
    /// promote a compiled page's confidence (§5.4)
    Promote {
        page: PathBuf,
        #[arg(long)]
        verified: bool,
        #[arg(long)]
        human_confirmed: bool,
    },
    /// write an approved answer back to wiki/answers/ (§6.6)
    Answer {
        question: String,
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value = "")]
        answer: String,
    },
    /// Studio-style export: brief | mindmap (§13)
    Studio {
        /// brief | mindmap
        kind: String,
        /// topic (brief) or root (mindmap)
        topic: String,
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
        #[arg(short = 'k', long, default_value_t = 8)]
        top: usize,
    },
    /// run the MCP server over stdio (§11)
    Mcp,
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
fn index_file(
    conn: &Connection,
    doc_path: &str,
    text: &str,
    layer: &str,
) -> rusqlite::Result<usize> {
    let (fm, body) = split_frontmatter(text);
    let title = fm_title(&fm).unwrap_or_else(|| doc_path.to_string());
    conn.execute(
        "INSERT OR REPLACE INTO documents(path,title,frontmatter,hash) VALUES(?,?,?,?)",
        params![doc_path, title, fm, ""],
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

    let flush = |pending: &mut Vec<String>, sec_path: &str, breadcrumb: &str,
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
            format!("{} > {}", breadcrumb,
                    sec_path.split('/').collect::<Vec<_>>().join(" > "))
        };
        let kind = if body.contains('|') { "table" } else { "paragraph" };
        conn.execute(
            "INSERT OR REPLACE INTO blocks(id,doc_path,section_id,type,breadcrumb,raw,\
             hash,page_range,meta,ordinal,layer) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            params![id, doc_path, sec_id, kind, crumb, body, "", None::<String>,
                    "{}", *ordinal, layer],
        )?;
        conn.execute("DELETE FROM blocks_fts WHERE id=?", params![&id])?;
        // store BOTH forms, mirroring the Python side
        conn.execute(
            "INSERT INTO blocks_fts(id,breadcrumb,raw) VALUES(?,?,?)",
            params![&id, format!("{} {}", crumb, cjk_pad(&crumb)),
                    format!("{} {}", body, cjk_pad(&body))],
        )?;
        Ok(())
    };

    // sec_path: current "/"-joined heading titles; "" means no heading yet.
    let mut sec_path = String::new();
    for line in body.lines() {
        let s = line.trim();
        if s.is_empty() {
            flush(&mut pending, &sec_path, &base_title,
                  &mut ordinal, &mut count, conn, doc_path)?;
            continue;
        }
        if let Some(heading) = s.strip_prefix('#') {
            flush(&mut pending, &sec_path, &base_title,
                  &mut ordinal, &mut count, conn, doc_path)?;
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
                let parent: Vec<String> =
                    stack[..stack.len() - 1].iter().map(|(_, t)| t.clone()).collect();
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
    flush(&mut pending, &sec_path, &base_title,
          &mut ordinal, &mut count, conn, doc_path)?;
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
    // links are DERIVED from Markdown (§5.2), so a rebuild must start from an
    // empty edge set — otherwise deleted wikilinks keep polluting the graph.
    conn.execute("DELETE FROM links", [])?;
    let mut total = 0usize;
    let mut seen_links: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();
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
            if layer == "wiki"
                && ingest::is_derived_page(&entry, &base)
            {
                continue;
            }
            let text = fs::read_to_string(&entry).unwrap_or_default();
            let n = index_file(&conn, &rel, &text, layer)?;
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

fn print_blocks(conn: &Connection, ids: &[String]) -> rusqlite::Result<()> {
    if ids.is_empty() {
        println!("(no hits)");
        return Ok(());
    }
    for (i, id) in ids.iter().enumerate() {
        let raw: String = conn
            .query_row("SELECT raw FROM blocks WHERE id=?", params![id], |r| r.get(0))
            .unwrap_or_default();
        let snippet: String = raw.chars().take(110).collect();
        println!("[{i}] {id}\n    {snippet}");
    }
    Ok(())
}

fn cmd_tree(path: &Path, query: &str) -> rusqlite::Result<()> {
    let conn = connect(path)?;
    let ids = retrieval::tree_search(&conn, query, 3)?;
    print_blocks(&conn, &ids)
}

fn cmd_full(path: &Path, max_chars: usize) -> rusqlite::Result<()> {
    let conn = connect(path)?;
    let ids = retrieval::full_corpus(&conn, max_chars)?;
    if ids.is_empty() {
        println!("(corpus over threshold — refusing to pretend a full read happened)");
        return Ok(());
    }
    println!("full corpus: {} blocks", ids.len());
    print_blocks(&conn, &ids)
}

fn cmd_query(path: &Path, query: &str, top: usize) -> rusqlite::Result<()> {
    let conn = connect(path)?;
    let rows = retrieval::hybrid_search(&conn, query, top, true)?;
    let ids: Vec<String> = rows.iter().map(|(i, _)| i.clone()).collect();
    let first: String = ids
        .first()
        .and_then(|id| conn.query_row("SELECT raw FROM blocks WHERE id=?", params![id], |r| r.get::<_, String>(0)).ok())
        .unwrap_or_default();
    let joined = first;
    let mut verdict = retrieval::grade(query, &joined);
    let mut escalated = false;
    // Self-Route (§6.2): escalate only when the corpus genuinely has a term.
    if verdict == "evidence_absent" && !joined.is_empty() {
        let full = retrieval::full_corpus(&conn, 200_000)?;
        if !full.is_empty() {
            escalated = true;
            let j2 = full
                .iter()
                .filter_map(|id| conn.query_row("SELECT raw FROM blocks WHERE id=?", params![id], |r| r.get::<_, String>(0)).ok())
                .collect::<Vec<_>>()
                .join(" ");
            verdict = retrieval::grade(query, &j2);
        }
    }
    println!("verdict: {verdict}  hits: {}  escalated: {escalated}", ids.len());
    print_blocks(&conn, &ids)
}

fn cmd_eval(path: &Path) -> rusqlite::Result<()> {
    // Seed golden set — must stay identical to koiosbase/eval/harness.py
    // SEED_CASES, otherwise the two shells report incomparable scores.
    let cases: Vec<(&str, Option<&str>, bool)> = vec![
        ("ACME 公司的营收是多少？", Some("report.md#财务分析/负债分析/1"), false),
        ("这家公司的现金流是多少？", Some("report.md#财务分析/现金流/1"), false),
        ("负债金额是多少？", Some("report.md#财务分析/负债分析/1"), false),
        ("火星基地 2030 年的预算是多少？", None, false),
        ("公司是否披露了季度分红政策？", None, true), // needs_llm -> skipped
    ];
    // Mirrors koiosbase/eval/harness.py run_case() one-for-one, including two
    // details that make the two shells' numbers actually comparable:
    //   * refusal cases run the FULL pipeline (full_query) and pass only when
    //     the answer carries the refusal marker and evidence is empty;
    //   * needs_llm cases still COUNT in total (Python does not skip them) so
    //     recall@1 has the same denominator on both sides.
    let top = 5usize;
    let conn = connect(path)?;
    let mut recall_ok = 0usize;
    let mut total = 0usize;
    let mut refusal_ok = 0usize;
    let mut refusal_total = 0usize;
    let mut needs_llm_skipped = 0usize;
    let mut citation_cov_sum = 0.0f64;
    for (q, want, needs_llm) in cases {
        total += 1;
        if want.is_none() {
            // ---- refusal / needs-llm branch (harness lines 91-111) ----
            // NOTE: both early returns in run_case happen BEFORE the citation
            // block, so refusal cases contribute 0.0 here too — matching them
            // matters or coverage lands on a different denominator.
            refusal_total += 1;
            let res = pipeline::full_query(&conn, q, top);
            if res.evidence.is_empty() && pipeline::is_refusal(&res.answer) {
                refusal_ok += 1;
                continue;
            }
            let detail = if needs_llm {
                needs_llm_skipped += 1;
                format!(
                    "[needs-llm] {q} -> keyword evidence cannot decide this in v0.1 ({} blocks)",
                    res.evidence.len()
                )
            } else {
                format!(
                    "[refusal] {q} -> {} evidence blocks, answer={}",
                    res.evidence.len(),
                    &res.answer.chars().take(40).collect::<String>()
                )
            };
            println!("  SKIP {detail}");
            continue;
        }
        // ---- factual branch (harness lines 113-126) ----
        let want = want.unwrap();
        let blocks = pipeline::retrieve_channel(&conn, q, top);
        let ids: Vec<String> = blocks.iter().map(|b| b.id.clone()).collect();
        if ids.iter().any(|i| i == want) {
            recall_ok += 1;
        } else {
            println!("  FAIL [factual] {q} -> got {:?}, want {want}", &ids[..ids.len().min(2)]);
        }
        // citation coverage measured on the assembled context (harness 128-130)
        let blocks2 = pipeline::retrieve_channel(&conn, q, top);
        let answered = pipeline::assemble(&blocks2, 6000);
        let (cited, considered) = pipeline::citation_coverage(&answered);
        citation_cov_sum += if considered == 0 {
            0.0
        } else {
            cited as f64 / considered as f64
        };
    }
    println!(
        "total={total} recall@1={:.3} refusal_acc={:.3} citation_cov={:.3} needs_llm={needs_llm_skipped}",
        recall_ok as f64 / total as f64,
        if refusal_total == 0 { 1.0 } else { refusal_ok as f64 / refusal_total as f64 },
        citation_cov_sum / total as f64
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { path } => cmd_init(&path.unwrap_or_else(|| PathBuf::from(".")))?,
        Cmd::Index { path } => cmd_index(&path.unwrap_or_else(|| PathBuf::from(".")))?,
        Cmd::Search { query, path, top } => cmd_search(&path, &query.join(" "), top)?,
        Cmd::Tree { query, path } => cmd_tree(&path, &query.join(" "))?,
        Cmd::Full { path, max_chars } => cmd_full(&path, max_chars)?,
        Cmd::Query { query, path, top } => cmd_query(&path, &query.join(" "), top)?,
        Cmd::Eval { path } => cmd_eval(&path)?,
        Cmd::Lint { path } => {
            let code = lint::cmd_lint(&path.unwrap_or_else(|| PathBuf::from(".")))?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Cmd::CheckClaim { claim, evidence } => {
            let v = lint::cross_judge(&claim, &evidence);
            println!("{v}");
            if v == "contradicted" {
                std::process::exit(1);
            }
        }
        Cmd::Retract {
            block_id,
            path,
            reason,
        } => {
            let vault = path.canonicalize().unwrap_or(path.clone());
            let conn = connect(&vault)?;
            match state::cascade_retraction(&conn, &block_id, &reason, Some(&vault)) {
                Ok(pages) => {
                    println!(
                        "{{\"block\": \"{}\", \"state\": \"retracted\", \"affected_pages\": {:?}}}",
                        block_id, pages
                    );
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Cmd::Compile { path } => {
            let vault = path.canonicalize().unwrap_or(path.clone());
            cmd_compile(&vault)?;
        }
        Cmd::Promote {
            page,
            verified,
            human_confirmed,
        } => match compile::promote(&page, verified, human_confirmed) {
            Ok(c) => println!("{c}"),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        Cmd::Studio {
            kind,
            topic,
            path,
            top,
        } => {
            let vault = path.canonicalize().unwrap_or(path.clone());
            let conn = connect(&vault)?;
            let out = match kind.as_str() {
                "brief" => studio::studio_brief(&conn, &vault, &topic, top),
                "mindmap" => studio::studio_mindmap(&conn, &vault, &topic),
                other => {
                    eprintln!("error: unknown studio kind: {other} (brief | mindmap)");
                    std::process::exit(1);
                }
            };
            match out {
                Ok(p) => println!("{}", p.display()),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Cmd::Mcp => {
            mcp::serve()?;
        }
        Cmd::Answer {
            question,
            path,
            answer,
        } => {
            let vault = path.canonicalize().unwrap_or(path.clone());
            let conn = connect(&vault)?;
            let ev = pipeline::retrieve_channel(&conn, &question, 8);
            // date-only, matching Python's answer write-back stamp
            let stamp = state::now_date();
            match compile::write_answer_page(&vault, &question, &answer, &ev, &stamp) {
                Ok(p) => println!("{}", p.display()),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
    Ok(())
}

/// Compile entities/ + sources/ from raw (§5.3) — mirrors compile/pipeline.py.
fn cmd_compile(vault: &Path) -> rusqlite::Result<()> {
    // Python's compile_vault() uses datetime.now(utc).date().isoformat() — a
    // DATE, not a full timestamp. Matching it keeps compiled pages byte-stable
    // across the two shells (§5.3: compiled pages are versioned Markdown).
    let stamp = state::now_date();
    let conn = connect(vault)?;
    let mut doc_paths: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT DISTINCT doc_path FROM blocks WHERE layer='raw'")?;
        let m = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for d in m.flatten() {
            doc_paths.push(d);
        }
    }
    let mut docs: Vec<(String, String, Vec<pipeline::Ev>)> = Vec::new();
    let mut sections_by_doc: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for rel in &doc_paths {
        // Route through the adapter: a PDF in raw/ parsed as Markdown would
        // produce garbage, so non-Markdown sources are skipped (§5.1).
        let p = vault.join("raw").join(rel);
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
        let mut sections: Vec<(String, String)> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id,raw,breadcrumb,doc_path FROM blocks WHERE doc_path=? AND layer='raw' \
                 ORDER BY ordinal",
            )?;
            let m = stmt.query_map(params![rel], |r| {
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
        {
            let mut stmt = conn.prepare(
                "SELECT title,id FROM sections WHERE doc_path=? ORDER BY ordinal",
            )?;
            let m = stmt.query_map(params![rel], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for t in m.flatten() {
                sections.push(t);
            }
        }
        sections_by_doc.insert(rel.clone(), sections);
        docs.push((rel.clone(), title, blocks));
    }

    let mut entities: HashMap<String, compile::Entity> = HashMap::new();
    let mut touched = 0usize;
    for (_rel, _title, blocks) in &docs {
        if blocks.is_empty() {
            continue;
        }
        let found = compile::extract_entities(blocks);
        touched += compile::affected_entities(&conn, &found).len();
        touched += compile::affected_concepts(&conn, blocks, 10).len();
        for (eid, ent) in found {
            if let Some(existing) = entities.get_mut(&eid) {
                existing.facts.extend(ent.facts);
            } else {
                entities.insert(eid, ent);
            }
        }
    }
    let n_pages = compile::write_entity_pages(vault, &entities, &stamp).unwrap_or(0);
    let n_sources =
        compile::write_source_pages(vault, &docs, &stamp, &sections_by_doc).unwrap_or(0);
    println!(
        "{{\"entities\": {}, \"entity_pages\": {n_pages}, \"source_pages\": {n_sources}, \
         \"affected_pages\": {touched}, \"stamp\": \"{stamp}\"}}",
        entities.len()
    );
    Ok(())
}
