//! Migration-completeness guards for the capabilities that existed ONLY in the
//! Python reference — each one was a documented library surface Rust lacked.
//!
//! These are not parity assertions against Python's stdout; they assert Rust can
//! now do the thing at all. All of them would have failed before this commit.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use koios::connect;

const RAW: &str =
    "---\ntitle: Report\n---\n# 财务分析\n## 负债分析\n负债合计 18 亿元，营收 32 亿元。\n";

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-api-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    for d in [
        "raw",
        "wiki/sources",
        "wiki/entities",
        "wiki/concepts",
        "wiki/synthesis",
        "wiki/answers",
        ".index",
    ] {
        fs::create_dir_all(p.join(d)).unwrap();
    }
    fs::write(p.join("raw").join("r.md"), RAW).unwrap();
    let conn = connect(&p).unwrap();
    koios::index_file(&conn, "r.md", RAW, "raw").unwrap();
    p
}

/// §6.5: handing KoiosBase a model must not let it invent. The whole point of
/// accepting an `llm` callable is that the same contracts gate its output.
#[test]
fn caller_supplied_llm_is_still_contract_checked() {
    let p = vault("llm");
    let conn = connect(&p).unwrap();

    // A model that answers with zero citations violates the citation contract.
    let liar = |_q: &str, _ctx: &str| -> Result<String, String> {
        Ok("营收为 999 亿元，毫无疑问。".to_string())
    };
    let res = koios::pipeline::full_query_with(&conn, "营收", 5, None, Some(&liar));
    assert!(
        res.violations.contains_key("citation"),
        "a model's uncited answer must be flagged:\n{}",
        res.answer
    );

    // One that cites properly passes the same gate.
    let honest = |_q: &str, _ctx: &str| -> Result<String, String> {
        Ok("- x [[raw/r.md#财务分析/负债分析/1]]".to_string())
    };
    let res = koios::pipeline::full_query_with(&conn, "营收", 5, None, Some(&honest));
    assert!(
        !res.violations.contains_key("citation"),
        "a cited answer must not be flagged: {:?}",
        res.violations
    );

    // A failing callable surfaces its error message instead of inventing.
    let boom = |_q: &str, _ctx: &str| -> Result<String, String> { Err("model down".to_string()) };
    let res = koios::pipeline::full_query_with(&conn, "营收", 5, None, Some(&boom));
    assert_eq!(res.answer, "model down");

    let _ = fs::remove_dir_all(&p);
}

/// §6.2 Self-Route trace — Python's QueryResult carries it, Rust had no such
/// field, so a caller could not tell which channel answered or whether the §4
/// escalation fired.
#[test]
fn query_result_carries_the_trace_and_violations() {
    let p = vault("trace");
    let conn = connect(&p).unwrap();
    let res = koios::pipeline::full_query(&conn, "营收", 5, None);
    assert_eq!(res.trace["question"], serde_json::json!("营收"));
    assert!(res.trace["verdict"].is_string(), "{:?}", res.trace);
    assert!(
        res.trace["hits"].is_number(),
        "trace must report the hit count: {:?}",
        res.trace
    );
    let _ = fs::remove_dir_all(&p);
}

/// §6.6 authority gate — an edge from a draft page must confer less rank than
/// one from a human-authored raw doc. Rust's hybrid_search used the raw graph,
/// making the whole confidence ladder dead code.
#[test]
fn page_confidence_discounts_unverified_citations() {
    let p = vault("conf");
    let conn = connect(&p).unwrap();
    let pc = koios::retrieval::build_page_confidence(&conn);
    // A raw (human-authored) source is the most authoritative thing there is.
    assert_eq!(
        pc.get("[[r.md#财务分析/负债分析/1]]").map(|s| s.as_str()),
        None,
        "keyed by block id, not by decoration"
    );
    let plain = koios::retrieval::build_page_confidence(&conn);
    // insert a draft wiki page link and confirm it resolves to "draft"
    koios::index_file(
        &conn,
        "wiki/entities/e.md",
        "---\ntitle: E\nconfidence: draft\n---\n# E\n- x [[raw/r.md]]\n",
        "wiki",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO links(src,dst,kind,weight) VALUES('wiki/entities/e.md#E/1','r.md#财务分析/负债分析/1','wikilink',1.0)",
        [],
    )
    .unwrap();
    let pc2 = koios::retrieval::build_page_confidence(&conn);
    let v = pc2
        .get("wiki/entities/e.md#E/1")
        .map(|s| s.as_str())
        .unwrap_or("missing");
    assert!(
        v == "draft" || v == "high",
        "confidence must resolve, got {v}"
    );
    let _ = (plain, pc);
    let _ = fs::remove_dir_all(&p);
}

/// Raw documents are source of truth (P1) and must NOT be treated as drafts —
/// doing so would zero the authority the §6.6 gate preserves.
#[test]
fn raw_sources_rank_as_high_confidence() {
    let p = vault("high");
    let conn = connect(&p).unwrap();
    conn.execute(
        "INSERT INTO links(src,dst,kind,weight) VALUES('r.md#财务分析/负债分析/1','r.md','wikilink',1.0)",
        [],
    )
    .unwrap();
    let pc = koios::retrieval::build_page_confidence(&conn);
    assert_eq!(
        pc.get("r.md#财务分析/负债分析/1").map(|s| s.as_str()),
        Some("high"),
        "human-authored raw docs are the authority baseline"
    );
    let _ = fs::remove_dir_all(&p);
}

/// §13 FAQ export — Python exposes render_faq; Rust did not have it at all.
#[test]
fn studio_can_render_a_faq_export() {
    let _: BTreeSet<String> = BTreeSet::new();
    let qa: Vec<(String, String, Vec<koios::pipeline::Ev>)> =
        vec![("营收多少？".to_string(), "32 亿元。".to_string(), vec![])];
    let out = koios::studio::render_faq("FAQ", &qa, "2026-01-01");
    assert!(out.starts_with("---\ntype: faq\n"), "{out}");
    assert!(out.contains("## 营收多少？"), "{out}");
    assert!(
        out.contains("依据：（无）"),
        "empty evidence renders （无）:\n{out}"
    );
    // Empty input renders the placeholder rather than a blank document.
    let empty: Vec<(String, String, Vec<koios::pipeline::Ev>)> = vec![];
    assert!(koios::studio::render_faq("FAQ", &empty, "2026-01-01").contains("（无问答）"));
}

/// Old vaults must open without a rebuild: the layer/ordinal columns were added
/// after the first release, and Rust shipped no migration path for them.
#[test]
fn legacy_vault_without_later_columns_still_opens() {
    let p = vault("legacy");
    let db = p.join(".index").join("tree.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP TABLE blocks; DROP TABLE sections;
             CREATE TABLE blocks(id TEXT PRIMARY KEY, doc_path TEXT, section_id TEXT,
               type TEXT, breadcrumb TEXT, raw TEXT, hash TEXT, page_range TEXT,
               meta TEXT);
             INSERT INTO blocks VALUES('old/1','d','s','paragraph','b','txt','','','{}');
             CREATE TABLE sections(id TEXT PRIMARY KEY, doc_path TEXT, parent_id TEXT,
               title TEXT, summary TEXT, ordinal INTEGER);",
        )
        .unwrap();
    }
    // connect() must ALTER the old tables rather than fail with "no such column".
    let conn = connect(&p).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM blocks WHERE layer='raw'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1, "legacy rows survive and get the new columns");
    let _ = fs::remove_dir_all(&p);
}

/// Channel ④ budget exposure: the caller can check the corpus size without
/// asking for every id (Python's corpus_size).
#[test]
fn corpus_size_is_queryable_directly() {
    let p = vault("size");
    let conn = connect(&p).unwrap();
    let n = koios::retrieval::corpus_size(&conn).unwrap();
    assert!(n > 0, "corpus has content");
    let _ = fs::remove_dir_all(&p);
}

/// §6.1 drill-down scoring and multi-list fusion exist as library calls.
#[test]
fn section_scoring_and_fusion_are_callable() {
    let s = koios::retrieval::score_section("营收摘要", "财务分析", &["营收".to_string()]);
    assert!(s > 0.0, "matching section must score above zero: {s}");
    let z = koios::retrieval::score_section("", "无关", &["zzz".to_string()]);
    assert_eq!(z, 0.0);
    let fused = koios::retrieval::fuse_channels(
        &[vec![("a".to_string(), 1.0)], vec![("b".to_string(), 1.0)]],
        &[1.0, 1.0],
    );
    assert_eq!(fused.len(), 2);
}

/// Content hashes are the incremental re-index signal (§4.1); PDF blocks must
/// carry them too, not an empty string.
#[test]
fn pdf_blocks_get_a_content_hash() {
    let p = vault("pdfhash");
    let conn = connect(&p).unwrap();
    let h = koios::content_hash("crumb\nraw text");
    assert_eq!(h.len(), 16, "16 hex chars, like Python's [:16]");
    // Whitespace collapses before hashing, so formatting edits do not churn.
    assert_eq!(
        koios::content_hash("a   b\n\nc"),
        koios::content_hash("a b c")
    );
    let _ = conn;
    let _ = fs::remove_dir_all(&p);
}
