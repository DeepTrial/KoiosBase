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
use koios::cmd_index;
use koios::compile;
use koios::connect;
use koios::fm_title;
use koios::lint;
use koios::mcp;
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
    Index {
        path: Option<PathBuf>,
        /// force a full rebuild. Python exposes this flag; it is a no-op there
        /// today because content hashes already make every rebuild correct, but
        /// accepting it keeps scripts portable between the two shells.
        #[arg(long)]
        full: bool,
    },
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
        /// shell command used as the generator; reads the context on stdin and
        /// prints the answer. Defaults to $KOIOS_LLM_CMD. Without one you get
        /// the built-in extractive answer, unchanged.
        #[arg(long = "llm-cmd")]
        llm_cmd: Option<String>,
        /// principal groups for ACL (§9.2), comma separated
        #[arg(long)]
        groups: Option<String>,
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
    /// cross-judge one claim against evidence (§10.3 L2)
    ///
    /// The Python CLI spells this `checkclaim`. Clap would otherwise rename the
    /// variant to `check-claim`, silently breaking host scripts ported from it.
    #[command(name = "checkclaim")]
    CheckClaim { claim: String, evidence: String },
    /// retract a block and cascade staleness to citing pages (§8.3)
    Retract {
        /// Python spells this `-b/--block`; accept that flag as well as the
        /// bare positional so scripts ported from the Python CLI keep running.
        #[arg(short = 'b', long = "block")]
        block_flag: Option<String>,
        block_id: Option<String>,
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
        /// vault directory; positional like index/lint, or -p/--path
        #[arg(short = 'p', long = "path")]
        path_flag: Option<PathBuf>,
        path: Option<PathBuf>,
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
        /// question; Python requires -q/--question, positional also accepted
        #[arg(short = 'q', long = "question")]
        question_flag: Option<String>,
        question: Option<String>,
        #[arg(short = 'p', long = "path", default_value = ".")]
        path: PathBuf,
        #[arg(short = 'a', long = "answer", default_value = "")]
        answer: String,
    },
    /// Studio-style export: brief | mindmap (§13)
    Studio {
        /// brief | mindmap
        kind: String,
        /// topic (brief) or root (mindmap); Python requires -t/--topic
        #[arg(short = 't', long = "topic")]
        topic_flag: Option<String>,
        topic: Option<String>,
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

fn cmd_init(path: &Path) -> std::io::Result<()> {
    // Delegates to lib::init_vault so the binary and integration tests exercise
    // the SAME scaffolding — the two copies used to drift (the bin wrote a
    // one-line AGENTS.md stub).
    koios::init_vault(path)?;
    println!("initialized KoiosBase vault at {}", path.display());
    Ok(())
}

/// Split a `--groups a,b` value into principals (§9.2).
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
    // Byte-identical to koiosbase/cli.py cmd_search()'s three-line form:
    //   [i] id / breadcrumb / snippet
    // Rust used to print `[doc_path] id` and drop the breadcrumb, so a hit
    // could not be turned into a [[wikilink]] without re-resolving it — and
    // the two shells' stdout could not be diffed directly.
    for (i, b) in ev.iter().enumerate() {
        let snippet: String = b.raw.replace('\n', " ").chars().take(110).collect();
        println!("[{}] {}\n    {}\n    {}", i, b.id, b.breadcrumb, snippet);
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

/// `koios query` — full pipeline with the Grader verdict (§6.2).
///
/// When a generator is configured (`--llm-cmd` or `$KOIOS_LLM_CMD`) this routes
/// through `full_query_with`, so the model's answer is gated by the same
/// citation and refusal contracts as the built-in one (§6.5). Without one it is
/// byte-for-byte the old output, which is what `eval` and the parity tests rely
/// on.
fn cmd_query(
    path: &Path,
    query: &str,
    top: usize,
    llm_cmd: Option<&str>,
    groups: Option<&str>,
) -> rusqlite::Result<()> {
    let conn = connect(path)?;
    let groups = parse_groups(groups);

    if let Some(cmd) = koios::llm::llm_cmd(llm_cmd) {
        let f = move |q: &str, ctx: &str| -> Result<String, String> {
            koios::llm::generate_with(&cmd, q, ctx)
        };
        let res = pipeline::full_query_with(&conn, query, top, groups.as_deref(), Some(&f));
        println!(
            "verdict: {}  hits: {}  escalated: {}",
            res.trace
                .get("verdict")
                .and_then(|v| v.as_str())
                .unwrap_or("enough"),
            res.evidence.len(),
            res.trace.get("escalated").is_some()
        );
        println!("{}", res.answer);
        if !res.violations.is_empty() {
            let mut keys: Vec<&String> = res.violations.keys().collect();
            keys.sort();
            eprintln!(
                "[contracts] {}",
                keys.iter()
                    .map(|k| format!("{k}={}", res.violations[*k]))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        return Ok(());
    }

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
    // Python's harness accumulates failures in `rep.failures` and cmd_eval
    // prints them AFTER the `total=` line. Rust printed them inline as each
    // case ran, so the two shells' stdout interleaved differently and any diff
    // looked like a behavioural change when only the ordering moved.
    let mut failures: Vec<(String, bool)> = Vec::new(); // (detail, is_needs_llm)
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
                    res.answer.chars().take(40).collect::<String>()
                )
            };
            if strict || !needs_llm {
                blocking += 1;
                failures.push((detail, false));
            } else {
                failures.push((detail, true));
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
            // Python formats this with a list repr (['a', 'b']); Rust's {:?} on
            // a slice prints ["a", "b"] with double quotes, which is enough to
            // make an otherwise-identical failure line differ byte-for-byte.
            let got = ids[..ids.len().min(2)]
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", ");
            failures.push((format!("[factual] {q} -> got [{got}], want {want}"), false));
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
    // Same two passes Python makes over rep.failures: blocking (strict-filtered)
    // FAIL lines first, then the SKIP lines for needs-llm cases.
    for (detail, is_needs_llm) in &failures {
        if !is_needs_llm {
            println!("  FAIL {detail}");
        }
    }
    for (detail, is_needs_llm) in &failures {
        if *is_needs_llm {
            println!("  SKIP {detail}");
        }
    }
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
        Cmd::Index { path, full } => {
            if full {
                // Python's --full does not drop tables either; rebuild already
                // regenerates every derived row from raw + wiki (P1).
                eprintln!("[index] --full accepted; rebuild is already complete");
            }
            cmd_index(&path.unwrap_or_else(|| PathBuf::from(".")))?
        }
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
        Cmd::Query {
            query,
            path,
            top,
            llm_cmd,
            groups,
        } => cmd_query(
            &path,
            &query.join(" "),
            top,
            llm_cmd.as_deref(),
            groups.as_deref(),
        )?,
        Cmd::Eval {
            path_flag,
            path,
            strict,
        } => {
            let p = path_flag.or(path).unwrap_or_else(|| PathBuf::from("."));
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
            block_flag,
            block_id,
            path,
            reason,
            groups,
        } => {
            let _ = parse_groups(groups.as_deref());
            let block_id = match block_flag.or(block_id) {
                Some(b) => b,
                None => {
                    eprintln!("error: -b/--block is required");
                    std::process::exit(2);
                }
            };
            let vault = path.canonicalize().unwrap_or(path.clone());
            let conn = connect(&vault)?;
            match state::cascade_retraction(&conn, &block_id, &reason, Some(&vault)) {
                Ok(pages) => {
                    // koiosbase/cli.py prints a sentence, not JSON — Rust emitted
                    // a JSON object, so every script parsing retract output had
                    // to special-case the Rust shell.
                    println!(
                        "retracted {block_id}; affected pages: {} {:?}",
                        pages.len(),
                        pages
                    );
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Cmd::Compile { path_flag, path } => {
            let p = path_flag.or(path).unwrap_or_else(|| PathBuf::from("."));
            let vault = p.canonicalize().unwrap_or(p);
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
            topic_flag,
            topic,
            path,
            top,
            groups,
        } => {
            let topic = match topic_flag.or(topic) {
                Some(t) => t,
                None => {
                    eprintln!("error: -t/--topic is required");
                    std::process::exit(2);
                }
            };
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
                // koiosbase/cli.py prints `wrote {path}`; Rust printed the bare
                // path, so the two shells' stdout differed on every export.
                Ok(p) => println!("wrote {}", p.display()),
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
            question_flag,
            question,
            path,
            answer,
        } => {
            // Python requires -q/--question; accept the positional form too so
            // scripts written against either shell work.
            let q = match question_flag.or(question) {
                Some(q) => q,
                None => {
                    eprintln!("error: --question is required");
                    std::process::exit(2);
                }
            };
            let vault = path.canonicalize().unwrap_or(path.clone());
            let conn = connect(&vault)?;
            // Python's cmd_answer retrieves with top=5 and reports the evidence
            // count; Rust used top=8 and printed only the path, so the same
            // command produced a different page AND a different line.
            let ev = pipeline::retrieve_channel(&conn, &q, 5, None);
            // date-only, matching Python's answer write-back stamp
            let stamp = state::now_date();
            match compile::write_answer_page(&vault, &q, &answer, &ev, &stamp) {
                Ok(p) => println!(
                    "wrote {} (confidence: draft, {} evidence links)",
                    p.display(),
                    ev.len()
                ),
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
        // Same adapter rule as collect_raw_docs: keep non-Markdown raw docs.
        // compile_vault() reaches them through parse_any(), so skipping here
        // made `koios compile` ignore PDFs that Python compiled — different
        // entity counts and missing sources/ pages from the same vault.
        let p = vault.join("raw").join(rel);
        let title: String = if p.extension().and_then(|x| x.to_str()) == Some("md") {
            match fs::read_to_string(&p) {
                Ok(t) => {
                    let (fm, _) = split_frontmatter(&t);
                    fm_title(&fm).unwrap_or_else(|| rel.clone())
                }
                Err(_) => rel.clone(),
            }
        } else {
            conn.query_row(
                "SELECT COALESCE(NULLIF(title,''),?) FROM documents WHERE path=?",
                params![rel, rel],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| rel.clone())
        };
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
    // koiosbase/cli.py cmd_compile prints `"compiled: " + " ".join(f"{k}={v}")`,
    // NOT JSON. Rust emitted a JSON object, so any script parsing this line had
    // to special-case the Rust shell.
    println!(
        "compiled: entities={} entity_pages={n_pages} source_pages={n_sources} affected_pages={touched} stamp={stamp}",
        entities.len()
    );
    Ok(())
}
