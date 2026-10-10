//! §5–§6 core pipeline — ported from tests/test_pipeline.py.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-pl-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

/// Mirrors tests/test_pipeline.py MD — same fixture, so both suites exercise an
/// identical corpus instead of drifting into different corpora.
const MD: &str = "---\ntitle: ACME 2024年报\nversion: 3\n---\n\n# 财务分析\n\n## 负债分析\nACME 公司 2024 年营收 32 亿元，负债合计 18 亿元。\n\n| 项目 | 金额 |\n|---|---|\n| 营收 | 32 亿 |\n| 负债 | 18 亿 |\n\n## 现金流\n经营性现金流为正，达到 7.5 亿元。\n";

fn seeded(name: &str) -> (PathBuf, rusqlite::Connection) {
    let v = vault(name);
    fs::write(v.join("raw").join("r.md"), MD).unwrap();
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();
    (v, conn)
}

/// test_parse_builds_tree_and_blocks
#[test]
fn parse_builds_tree_and_blocks() {
    let (_v, conn) = seeded("tree");
    let blocks: i64 = conn
        .query_row("SELECT COUNT(*) FROM blocks WHERE layer='raw'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let sections: i64 = conn
        .query_row("SELECT COUNT(*) FROM sections", [], |r| r.get(0))
        .unwrap();
    assert!(blocks >= 2, "paragraph blocks expected, got {blocks}");
    assert!(sections >= 2, "nested headings -> sections, got {sections}");
    // parent relation must be populated for nested headings
    let with_parent: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sections WHERE parent_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(with_parent >= 1, "a nested section must have a parent");
}

/// test_frontmatter_and_breadcrumb
#[test]
fn frontmatter_and_breadcrumb() {
    let (_v, conn) = seeded("fm");
    let fm: String = conn
        .query_row(
            "SELECT frontmatter FROM documents WHERE path='r.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(fm.contains("ACME"), "frontmatter stored as JSON: {fm}");
    let crumb: String = conn
        .query_row(
            "SELECT breadcrumb FROM blocks WHERE doc_path='r.md' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        crumb.contains("财务分析"),
        "breadcrumb must carry the heading path: {crumb}"
    );
}

/// test_index_rebuild_counts_blocks
#[test]
fn index_rebuild_counts_blocks() {
    let v = vault("rebuild");
    fs::write(v.join("raw").join("r.md"), MD).unwrap();
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();
    let n1: i64 = conn
        .query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))
        .unwrap();
    koios::cmd_index(&v).unwrap();
    let n2: i64 = conn
        .query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n1, n2, "rebuild is idempotent, never grows the block set");
}

/// test_fts_finds_exact_term
#[test]
fn fts_finds_exact_term() {
    let (_v, conn) = seeded("fts");
    // CJK must be searchable despite unicode61 treating a whole run as one
    // token — that is exactly what cjk_pad() exists to fix (test on two terms
    // because Python asserts both fts and hybrid).
    let rows = koios::retrieval::fts_search(&conn, "营收", 8).unwrap_or_default();
    assert!(!rows.is_empty(), "BM25 channel returned nothing for 营收");
    let hits = koios::pipeline::retrieve_channel(&conn, "现金流", 8, None);
    assert!(!hits.is_empty(), "hybrid recall failed for 现金流");
}

/// test_retrieve_and_assemble_respects_budget
#[test]
fn retrieve_and_assemble_respects_budget() {
    let (_v, conn) = seeded("budget");
    let ev = koios::pipeline::retrieve_channel(&conn, "revenue", 8, None);
    let ctx = koios::pipeline::assemble(&ev, 10);
    assert!(
        ctx.chars().count() <= 10 + 64,
        "assemble must honour the char budget; got {} chars",
        ctx.chars().count()
    );
}

/// test_query_with_no_evidence_refuses
#[test]
fn query_with_no_evidence_refuses() {
    let (_v, conn) = seeded("refuse");
    let res = koios::pipeline::full_query(&conn, "zzzznotacorpusterm", 8, None);
    assert!(
        koios::pipeline::is_refusal(&res.answer),
        "no evidence -> refusal marker; got {:?}",
        res.answer
    );
}

/// test_citation_contract_flags_uncited_answer
#[test]
fn citation_contract_flags_uncited_answer() {
    let (_v, _conn) = seeded("cite");
    let v = koios::pipeline::check_contracts("a claim with no citation.", &[]);
    assert!(
        !v.is_empty(),
        "an uncited assertion with no evidence must violate the citation contract"
    );
}

/// test_ppr_runs_on_subgraph
#[test]
fn ppr_runs_on_subgraph() {
    let (v, conn) = seeded("subgraph");
    fs::write(
        v.join("raw").join("s.md"),
        "---\ntitle: S\n---\n# Sec\nSee [[r.md]] for revenue.\n",
    )
    .unwrap();
    koios::cmd_index(&v).unwrap();
    let graph = koios::retrieval::load_graph(&conn).unwrap();
    assert!(!graph.is_empty(), "wikilinks must produce graph edges");
    let out = koios::retrieval::personalized_pagerank(&["r.md".to_string()], &graph, 0.85, 20);
    assert!(!out.is_empty(), "PPR must converge on a non-empty result");
}

/// test_cross_family_judge_numbers
#[test]
fn cross_family_judge_numbers() {
    // deterministic placeholder: only numbers are decidable
    assert_eq!(
        koios::lint::cross_judge("revenue was 32", "revenue 32"),
        "entailed"
    );
    assert_eq!(
        koios::lint::cross_judge("revenue was 32", "revenue 99"),
        "contradicted"
    );
    assert_eq!(
        koios::lint::cross_judge("colour is nice", "the sky"),
        "unknown",
        "non-numeric relations must honestly return unknown (§8.4)"
    );
}

/// test_l1_detects_unsourced_wiki_assertion
#[test]
fn l1_detects_unsourced_wiki_assertion() {
    let (v, conn) = seeded("l1");
    fs::write(
        v.join("wiki").join("w.md"),
        "---\ntitle: W\nconfidence: draft\n---\n- Berlin was great in 2024.\n",
    )
    .unwrap();
    koios::cmd_index(&v).unwrap();
    let findings = koios::lint::run_l1(&conn);
    let unsourced = findings
        .iter()
        .find(|(k, _)| *k == "unsourced_assertions")
        .map(|(_, val)| val.clone())
        .unwrap_or_default();
    assert!(
        !unsourced.is_empty(),
        "a wiki claim with no [[wikilink]] must be flagged; got {unsourced:?}"
    );
}

/// test_derived_pages_are_indexed_and_idempotent
#[test]
fn derived_pages_are_indexed_and_idempotent() {
    let v = vault("derived");
    fs::write(v.join("raw").join("r.md"), MD).unwrap();
    koios::cmd_index(&v).unwrap();
    let n1: i64 = {
        let c = connect(&v).unwrap();
        c.query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))
            .unwrap()
    };
    koios::cmd_index(&v).unwrap();
    koios::cmd_index(&v).unwrap();
    let n3: i64 = {
        let c = connect(&v).unwrap();
        c.query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(n1, n3, "derived pages regenerate deterministically");
    assert!(n1 > 1, "derived pages must contribute blocks, got {n1}");
}

/// test_compile_reads_only_raw_layer
///
/// The compile step must not feed its own output back in, or page counts grow
/// without bound across rebuilds (§5.3).
#[test]
fn compile_reads_only_raw_layer() {
    let (v, conn) = seeded("layerraw");
    let (docs, _secs) = koios::collect_raw_docs(&conn, &v).unwrap();
    for (rel, _t, _b) in &docs {
        assert!(
            !rel.starts_with("sources/") && !rel.starts_with("entities/"),
            "compile input must be raw only, saw {rel}"
        );
    }
    assert!(!docs.is_empty(), "raw docs must be collected");
}
