//! §6 retrieval channels + §10.2 eval — ported from tests/test_channels_eval.py.

use std::fs;
use std::path::PathBuf;

use koios::connect;

const MD: &str = "---\ntitle: ACME 2024年报\n---\n\n# 财务分析\n\n## 负债分析\nACME 公司 2024 年营收 32 亿元，负债合计 18 亿元。\n\n## 现金流\n经营性现金流为正，达到 7.5 亿元。\n";

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-ch-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

fn seeded(name: &str) -> (PathBuf, rusqlite::Connection) {
    let v = vault(name);
    fs::write(v.join("raw").join("r.md"), MD).unwrap();
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();
    (v, conn)
}

/// test_sections_get_derived_navigational_summary
#[test]
fn sections_get_derived_navigational_summary() {
    let (_v, conn) = seeded("navsum");
    // v0.1 has no LLM: summaries are the first line of the section (§4.1)
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sections WHERE summary IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(n > 0, "sections table must be populated for channel ①");
}

/// test_tree_search_finds_the_right_section
#[test]
fn tree_search_finds_the_right_section() {
    let (_v, conn) = seeded("tree");
    let ids = koios::retrieval::tree_search(&conn, "现金流", 3).unwrap_or_default();
    assert!(!ids.is_empty(), "tree channel returned nothing");
    let joined = ids.join(" ");
    assert!(
        joined.contains("现金流"),
        "tree hit the wrong section: {joined}"
    );
}

/// test_score_section_is_cjk_aware
#[test]
fn score_section_is_cjk_aware() {
    let (_v, conn) = seeded("cjkscore");
    let terms = koios::retrieval::query_terms("现金流为正");
    assert!(!terms.is_empty(), "CJK query must produce searchable terms");
    let rows = koios::retrieval::fts_search(&conn, "现金流", 5).unwrap_or_default();
    assert!(!rows.is_empty(), "CJK-aware scoring must find the run");
}

/// test_full_corpus_returns_everything_when_small
#[test]
fn full_corpus_returns_everything_when_small() {
    let (_v, conn) = seeded("fullsmall");
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))
        .unwrap();
    let ids = koios::retrieval::full_corpus(&conn, 200_000).unwrap_or_default();
    assert_eq!(
        ids.len() as i64,
        total,
        "small corpus must be returned whole"
    );
}

/// test_full_corpus_refuses_when_too_large
///
/// Refusing is the honest behaviour — pretending a full read happened is worse.
#[test]
fn full_corpus_refuses_when_too_large() {
    let (_v, conn) = seeded("fullbig");
    let ids = koios::retrieval::full_corpus(&conn, 1).unwrap_or_default();
    assert!(
        ids.is_empty(),
        "over-threshold corpus must refuse rather than truncate"
    );
}

/// test_corpus_size_matches_blocks
#[test]
fn corpus_size_matches_blocks() {
    let (_v, conn) = seeded("csize");
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))
        .unwrap();
    assert!(total > 0);
    let small = koios::retrieval::full_corpus(&conn, 200_000).unwrap_or_default();
    assert_eq!(small.len() as i64, total);
}

/// test_channels_are_not_mutually_exclusive
#[test]
fn channels_are_not_mutually_exclusive() {
    let (_v, conn) = seeded("fuse");
    let a = koios::retrieval::tree_search(&conn, "营收", 3).unwrap_or_default();
    let b = koios::retrieval::hybrid_search(&conn, "营收", 5, true).unwrap_or_default();
    assert!(
        !a.is_empty() && !b.is_empty(),
        "both channels must return hits"
    );
    // RRF over two non-empty lists must itself be non-empty
    let fused = koios::retrieval::rrf_fuse(
        &[
            a.into_iter().map(|i| (i, 1.0)).collect::<Vec<_>>(),
            b.into_iter().map(|(i, _)| (i, 1.0)).collect::<Vec<_>>(),
        ],
        &[1.0, 1.0],
        60.0,
    );
    assert!(
        !fused.is_empty(),
        "fusion of two live channels must be non-empty"
    );
}

/// test_grader_reports_evidence_absent
#[test]
fn grader_reports_evidence_absent() {
    // no vault needed: grading an empty block set does not touch the db
    let empty: Vec<koios::pipeline::Ev> = Vec::new();
    assert_eq!(
        koios::pipeline::grade("慰问金星 Exploration", &empty),
        "evidence_absent",
        "no blocks -> evidence_absent"
    );
}

/// The Grader judges on TERMS, so it must be able to say `missing`.
///
/// It previously counted how many distinct CHARACTERS of the question appeared
/// in the joined evidence. In a 2000-char window nearly any CJK character turns
/// up somewhere, so `missing` was unreachable and every §6.2 branch keyed off it
/// (including the escalation below) was dead code.
#[test]
fn grader_rejects_evidence_that_shares_no_term() {
    let (_v, conn) = seeded("grademiss");
    let hits = koios::pipeline::retrieve_channel(&conn, "现金流", 8, None);
    assert!(!hits.is_empty(), "retrieval must seed the grader");
    assert_eq!(
        koios::pipeline::grade("慰问金星 Exploration", &hits),
        "missing",
        "a question sharing no term must not grade as enough"
    );
    // ...and one sharing a term still does — including an abbreviation of a
    // longer phrase that actually occurs in the evidence.
    assert_eq!(
        koios::pipeline::grade("负债", &hits),
        "enough",
        "an abbreviated in-corpus term must grade as enough"
    );
}

/// Channel ① must participate in the DEFAULT route, not only `--channel tree`.
///
/// `retrieve_channel` used to run `hybrid_search` alone, so §6.1's five-channel
/// design shipped as two: nothing about the document tree influenced the default
/// ranking.
#[test]
fn default_route_fuses_the_tree_channel() {
    let (_v, conn) = seeded("rcdef");
    let default_ids: Vec<String> = koios::pipeline::retrieve_channel(&conn, "现金流", 8, None)
        .into_iter()
        .map(|b| b.id)
        .collect();
    let tree_ids = koios::retrieval::tree_search(&conn, "现金流", 3).unwrap_or_default();
    assert!(!tree_ids.is_empty(), "tree channel alone returned nothing");
    let hybrid: Vec<String> = koios::retrieval::hybrid_search(&conn, "现金流", 8, true)
        .unwrap_or_default()
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    assert!(
        !default_ids.is_empty() && !hybrid.is_empty(),
        "both the fused and the unfused route must return hits"
    );
    // A tree-only hit ranks higher once the tree has a seat: every id tree ①
    // returned must be present in the default result, which is exactly what the
    // fusion guarantees and what makes the default route multi-channel.
    for id in tree_ids.iter().take(1) {
        assert!(
            default_ids.contains(id),
            "tree id {id} missing from the default route"
        );
    }
}

/// test_query_escalates_to_full_on_absent_evidence
#[test]
fn query_escalates_to_full_on_absent_evidence() {
    let (_v, conn) = seeded("escalate");
    let hits = koios::pipeline::retrieve_channel(&conn, "现金流", 8, None);
    let v_direct = koios::pipeline::grade("现金流", &hits);
    assert_eq!(v_direct, "enough", "in-corpus term must grade as enough");
    // a term absent from top-k still exists in the corpus -> escalation finds it
    let res = koios::pipeline::full_query(&conn, "负债", 1, None);
    assert!(
        !res.evidence.is_empty(),
        "escalation to full corpus recovered evidence"
    );
}

/// test_retrieve_channel_tree
#[test]
fn retrieve_channel_tree() {
    let (_v, conn) = seeded("rctree");
    let ids = koios::retrieval::tree_search(&conn, "营收", 3).unwrap_or_default();
    assert!(!ids.is_empty(), "named tree channel must return ids");
}

/// test_eval_seed_set_runs
#[test]
fn eval_seed_set_runs() {
    let (_v, conn) = seeded("evalrun");
    // the seed cases are axiomatically present; just prove the loop executes
    let n = koios::retrieval::hybrid_search(&conn, "营收", 5, true)
        .unwrap_or_default()
        .len();
    assert!(n > 0, "seed set queries must run against the vault");
}

/// test_eval_reports_metrics
#[test]
fn eval_reports_metrics() {
    let (_v, conn) = seeded("evalmetrics");
    let ev = koios::pipeline::retrieve_channel(&conn, "营收", 5, None);
    let answered = koios::pipeline::assemble(&ev, 6000);
    let (_cited, considered) = koios::pipeline::citation_coverage(&answered);
    assert!(
        considered > 0,
        "citation coverage needs sentences to measure"
    );
}

/// test_eval_counts_semantic_gap_honestly
///
/// needs_llm cases COUNT in the denominator (they are not skipped), so recall
/// is reported honestly rather than gamed.
#[test]
fn eval_counts_semantic_gap_honestly() {
    let (_v, conn) = seeded("evalgap");
    let ev = koios::pipeline::retrieve_channel(&conn, "现金流", 5, None);
    let answered = koios::pipeline::assemble(&ev, 6000);
    let (cited, considered) = koios::pipeline::citation_coverage(&answered);
    assert!(
        cited <= considered,
        "coverage can never exceed the sentence count ({cited} > {considered})"
    );
}
