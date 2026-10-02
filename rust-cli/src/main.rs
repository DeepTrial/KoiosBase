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

use koios::acl;
use koios::cjk_pad;
use koios::cmd_index;
use koios::compile;
use koios::connect;
use koios::fm_title;
use koios::ingest;
use koios::lint;
use koios::mcp;
use koios::pdf;
use koios::pipeline;
use koios::retrieval;
use koios::split_frontmatter;
use koios::state;
use koios::studio;

use clap::{Parser, Subcommand};
use rusqlite::{params, Connection};

/// The SCHEMA and `connect()` live in `koios` (src/lib.rs) — see the note there
/// for why there is exactly one copy.

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
        /// principal groups for ACL (§9.2), comma separated
        #[arg(long)]
        groups: Option<String>,
        /// force a retrieval channel: hybrid | tree | full | graph (§6.1)
        #[arg(short = 'c', long, value_parser = ["hybrid", "tree", "full", "graph"])]
        channel: Option<String>,
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
        /// vault directory (positional, like index/lint); -p/--path also works
        #[arg(short = 'p', long = "path")]
        path_flag: Option<PathBuf>,
        path: Option<PathBuf>,
        /// fail on needs-llm cases too
        #[arg(long)]
        strict: bool,
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
        /// principal groups for ACL (§9.2), comma separated
        #[arg(long)]
        groups: Option<String>,
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
        /// principal groups for ACL (§9.2), comma separated
        #[arg(long)]
        groups: Option<String>,
    },
    /// run the MCP server over stdio (§11)
    Mcp,
}

/// Insert spaces between CJK characters — mirrors Python's cjk_pad().

fn cmd_init(path: &Path) -> std::io::Result<()> {
    // Delegates to lib::init_vault so the binary and integration tests exercise
    // the SAME scaffolding — the two copies used to drift (the bin wrote a
    // one-line AGENTS.md stub).
    koios::init_vault(path)?;
    println!("initialized KoiosBase vault at {}", path.display());
    Ok(())
}

/// Minimal Markdown splitter: headings become sections, blank-line separated
/// paragraphs become blocks. Tables/code are kept whole (P2).
/// Index one Markdown document. PDFs go through `index_pdf` instead (§5.1:
/// a new format = one new adapter, never a special case inside this function).

/// Collect (rel_path, title, raw blocks) + per-doc sections for every Markdown
/// file in raw/. Shared by `index` and `compile` so both see the same inputs.
///
/// This exists because `index` must REGENERATE sources/ before the wiki walk —
/// otherwise the first pass counts only the raw blocks and the second counts the
/// derived ones too, making the command look non-idempotent (the Python side had
/// exactly this bug; see build_index's "Derived pages are generated BEFORE the
/// wiki walk" comment).

/// Same walk, but accepting every format an adapter handles (§5.1).

/// Index a PDF: one Section per page, Blocks tagged with page_range (§4.1).
///
/// `page_range` lives in the blocks.page_range column as a JSON pair, matching
/// `json.dumps(b.page_range)` on the Python side.

/// Split a `--groups a,b` value into principals (mirrors cli.parse_groups).
fn parse_groups(raw: Option<&str>) -> Option<Vec<String>> {
    raw.map(|g| {
        g.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    })
    .filter(|v: &Vec<String>| !v.is_empty())
}

/// Dispatch a named retrieval channel to block ids (§6.1).
///
/// Mirrors koiosbase/retrieval/channels.py retrieve_channel() one-for-one,
/// including the `full` slice (`[: limit * 20]`) so the same channel yields the
/// same number of candidates on both shells.
fn channel_ids(conn: &rusqlite::Connection, channel: &str, query: &str, top: usize) -> Vec<String> {
    match channel {
        "tree" => koios::retrieval::tree_search(conn, query, 3).unwrap_or_default(),
        "full" => {
            let mut ids = koios::retrieval::full_corpus(conn, 200_000).unwrap_or_default();
            ids.truncate(top * 20);
            ids
        }
        "hybrid" => koios::retrieval::hybrid_search(conn, query, top, false)
            .unwrap_or_default()
            .into_iter()
            .map(|(i, _)| i)
            .collect(),
        "graph" => koios::retrieval::hybrid_search(conn, query, top, true)
            .unwrap_or_default()
            .into_iter()
            .map(|(i, _)| i)
            .collect(),
        other => {
            eprintln!("error: unknown channel: {other}");
            std::process::exit(2);
        }
    }
}

fn cmd_search(
    path: &Path,
    query: &str,
    top: usize,
    groups: Option<Vec<String>>,
    channel: Option<&str>,
) -> rusqlite::Result<()> {
    let conn = connect(path)?;
    // Route through retrieve_channel so the ACL applies. The previous raw-FTS
    // ladder here bypassed it completely, so `koios search` handed restricted
    // blocks to whoever asked.
    //
    // `--channel` selects ONE route (§6.1) exactly like Python's
    // koiosbase/query/pipeline.py retrieve(). Without this the Rust shell had
    // no way to reach channels ①/③/④ at all.
    let ev = if let Some(ch) = channel {
        let ids = channel_ids(&conn, ch, query, top);
        let mut out: Vec<pipeline::Ev> = ids
            .iter()
            .filter_map(|i| pipeline::get_block(&conn, i))
            .collect();
        out = acl::filter_blocks(&conn, out, groups.as_deref());
        out = state::filter_visible(&conn, out);
        state::apply_disposition(&conn, out, false)
    } else {
        pipeline::retrieve_channel(&conn, query, top, groups.as_deref())
    };
    for b in &ev {
        println!("[{}] {}\n    {}", b.doc_path, b.id, {
            let s: String = b.raw.chars().take(110).collect();
            s
        });
    }
    if ev.is_empty() {
        println!("(no hits)");
    }
    Ok(())
}

fn print_blocks(conn: &Connection, ids: &[String]) -> rusqlite::Result<()> {
    if ids.is_empty() {
        println!("(no hits)");
        return Ok(());
    }
    for (i, id) in ids.iter().enumerate() {
        let raw: String = conn
            .query_row("SELECT raw FROM blocks WHERE id=?", params![id], |r| {
                r.get(0)
            })
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
        .and_then(|id| {
            conn.query_row("SELECT raw FROM blocks WHERE id=?", params![id], |r| {
                r.get::<_, String>(0)
            })
            .ok()
        })
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
                .filter_map(|id| {
                    conn.query_row("SELECT raw FROM blocks WHERE id=?", params![id], |r| {
                        r.get::<_, String>(0)
                    })
                    .ok()
                })
                .collect::<Vec<_>>()
                .join(" ");
            verdict = retrieval::grade(query, &j2);
        }
    }
    println!(
        "verdict: {verdict}  hits: {}  escalated: {escalated}",
        ids.len()
    );
    print_blocks(&conn, &ids)
}

fn cmd_eval(path: &Path, strict: bool) -> rusqlite::Result<()> {
    // Seed golden set — must stay identical to koiosbase/eval/harness.py
    // SEED_CASES, otherwise the two shells report incomparable scores.
    let cases: Vec<(&str, Option<&str>, bool)> = vec![
        (
            "ACME 公司的营收是多少？",
            Some("report.md#财务分析/负债分析/1"),
            false,
        ),
        (
            "这家公司的现金流是多少？",
            Some("report.md#财务分析/现金流/1"),
            false,
        ),
        (
            "负债金额是多少？",
            Some("report.md#财务分析/负债分析/1"),
            false,
        ),
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
    // Python's cmd_eval treats a needs-llm failure as blocking ONLY under
    // --strict; without the flag those are known v0.1 gaps, not regressions.
    let mut blocking = 0usize;
    for (q, want, needs_llm) in cases {
        total += 1;
        if want.is_none() {
            // ---- refusal / needs-llm branch (harness lines 91-111) ----
            // NOTE: both early returns in run_case happen BEFORE the citation
            // block, so refusal cases contribute 0.0 here too — matching them
            // matters or coverage lands on a different denominator.
            refusal_total += 1;
            let res = pipeline::full_query(&conn, q, top, None);
            // REFUSAL cases DO contribute coverage, measured on the ANSWER.
            // The Python harness records this before returning (harness
            // comment: a refusal has no citation, which is correct, and
            // leaving the sum at 0 would average over fewer than `total`
            // cases and inflate the metric). Missing this is what made the
            // two shells print different citation_cov on identical vaults.
            {
                let (cited, n) = pipeline::citation_coverage(&res.answer);
                citation_cov_sum += if n == 0 { 0.0 } else { cited as f64 / n as f64 };
            }
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
            if strict || !needs_llm {
                blocking += 1;
                println!("  FAIL {detail}");
            } else {
                println!("  SKIP {detail}");
            }
            continue;
        }
        // ---- factual branch (harness lines 113-126) ----
        let want = want.unwrap();
        let blocks = pipeline::retrieve_channel(&conn, q, top, None);
        let ids: Vec<String> = blocks.iter().map(|b| b.id.clone()).collect();
        if ids.iter().any(|i| i == want) {
            recall_ok += 1;
        } else {
            blocking += 1;
            println!(
                "  FAIL [factual] {q} -> got {:?}, want {want}",
                &ids[..ids.len().min(2)]
            );
        }
        // §10.2: coverage is measured on the ANSWER, not the assembled context.
        // Python made this change deliberately (harness comment) because
        // CITATION_RE looks for [[wikilink]] while the context format is a
        // bare `[id]` line — measuring the context pinned coverage near 0 and
        // the metric could never detect a regression.
        let res = pipeline::full_query(&conn, q, top, None);
        let (cited, considered) = pipeline::citation_coverage(&res.answer);
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
    // Mirrors Python's cmd_eval exit code: non-zero iff anything blocked.
    if blocking > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { path } => cmd_init(&path.unwrap_or_else(|| PathBuf::from(".")))?,
        Cmd::Index { path } => cmd_index(&path.unwrap_or_else(|| PathBuf::from(".")))?,
        Cmd::Search {
            query,
            path,
            top,
            groups,
            channel,
        } => cmd_search(
            &path,
            &query.join(" "),
            top,
            parse_groups(groups.as_deref()),
            channel.as_deref(),
        )?,
        Cmd::Tree { query, path } => cmd_tree(&path, &query.join(" "))?,
        Cmd::Full { path, max_chars } => cmd_full(&path, max_chars)?,
        Cmd::Query { query, path, top } => cmd_query(&path, &query.join(" "), top)?,
        Cmd::Eval {
            path_flag,
            path,
            strict,
        } => {
            let p = path_flag
                .or(path)
                .unwrap_or_else(|| PathBuf::from("."));
            cmd_eval(&p, strict)?
        }
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
            groups,
        } => {
            let _ = parse_groups(groups.as_deref());
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
            groups,
        } => {
            let vault = path.canonicalize().unwrap_or(path.clone());
            let conn = connect(&vault)?;
            let parsed = parse_groups(groups.as_deref());
            let out = match kind.as_str() {
                "brief" => studio::studio_brief(&conn, &vault, &topic, top, parsed.as_deref()),
                "mindmap" => studio::studio_mindmap(&conn, &vault, &topic, parsed.as_deref()),
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
            let ev = pipeline::retrieve_channel(&conn, &question, 8, None);
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
            let mut stmt =
                conn.prepare("SELECT title,id FROM sections WHERE doc_path=? ORDER BY ordinal")?;
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
