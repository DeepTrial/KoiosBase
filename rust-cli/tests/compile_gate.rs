//! §5.3 / §5.4 compile + promotion gate — ported from tests/test_compile_gate.py.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-cg-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

fn seeded(name: &str) -> (PathBuf, rusqlite::Connection) {
    let v = vault(name);
    fs::write(
        v.join("raw").join("r.md"),
        "---\ntitle: T\n---\n# Sec\nACME Corp revenue was 32 yuan. Foo Ltd said hello.\n",
    )
    .unwrap();
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();
    (v, conn)
}

fn blocks_of(conn: &rusqlite::Connection) -> Vec<koios::pipeline::Ev> {
    let mut stmt = conn
        .prepare("SELECT id,raw,breadcrumb,doc_path FROM blocks WHERE layer='raw' ORDER BY ordinal")
        .unwrap();
    stmt.query_map([], |r| {
        Ok(koios::pipeline::Ev {
            id: r.get(0)?,
            raw: r.get::<_, String>(1).unwrap_or_default(),
            breadcrumb: r.get::<_, String>(2).unwrap_or_default(),
            doc_path: r.get::<_, String>(3).unwrap_or_default(),
        })
    })
    .unwrap()
    .filter_map(Result::ok)
    .collect()
}

/// test_entity_extraction_finds_orgs
#[test]
fn entity_extraction_finds_orgs() {
    let (_v, conn) = seeded("ents");
    let blocks = blocks_of(&conn);
    let ents = koios::compile::extract_entities(&blocks);
    // ids are slugified, so match the slug rather than the raw surface form
    assert!(
        ents.values().any(|e| e.name.contains("ACME")) || ents.keys().any(|k| k.contains("acme")),
        "orgs must be extracted; got {:?}",
        ents.keys().collect::<Vec<_>>()
    );
}

/// test_entity_page_carries_raw_citations
///
/// Every factual line on a compiled page must point back at raw (P6).
#[test]
fn entity_page_carries_raw_citations() {
    let (_v, conn) = seeded("cit");
    let blocks = blocks_of(&conn);
    let ents = koios::compile::extract_entities(&blocks);
    let ent = ents.values().next().expect("at least one entity");
    let page = koios::compile::render_entity_page(ent, "2026-01-01");
    assert!(
        page.contains("[[") && page.contains("]]"),
        "entity page must cite its raw source:\n{page}"
    );
}

/// test_compile_writes_entity_pages
#[test]
fn compile_writes_entity_pages() {
    let (v, conn) = seeded("wep");
    let blocks = blocks_of(&conn);
    let ents = koios::compile::extract_entities(&blocks);
    let n = koios::compile::write_entity_pages(&v, &ents, "2026-01-01").unwrap();
    assert!(n > 0, "compile must write at least one entity page");
    let written: Vec<_> = fs::read_dir(v.join("wiki").join("entities"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!written.is_empty(), "entity dir must not be empty");
}

/// test_promotion_requires_verification
#[test]
fn promotion_requires_verification() {
    let (_v, _conn) = seeded("promo");
    assert!(
        !koios::compile::can_promote("draft", false, false),
        "neither flag set -> no promotion"
    );
    assert!(
        koios::compile::can_promote("draft", true, false),
        "cross-family verification promotes"
    );
    assert!(
        koios::compile::can_promote("draft", false, true),
        "human confirmation promotes"
    );
}

/// test_promotion_is_stepwise
///
/// draft -> reviewed -> verified. Skipping a rung would let a page reach
/// "verified" on a single weak signal.
#[test]
fn promotion_is_stepwise() {
    let v = vault("step");
    let p = v.join("wiki").join("e.md");
    fs::write(&p, "---\ntitle: E\nconfidence: draft\n---\nbody\n").unwrap();
    assert_eq!(koios::compile::read_confidence(&p), "draft");
    // §5.4 ladder is draft -> medium -> high, one rung per promotion
    let after1 = koios::compile::promote(&p, true, false).unwrap();
    assert_eq!(after1, "medium", "draft -> medium, never straight to high");
    let after2 = koios::compile::promote(&p, true, false).unwrap();
    assert_eq!(after2, "high");
    let after3 = koios::compile::promote(&p, true, false).unwrap();
    assert_eq!(after3, "high", "high is terminal");
}

/// test_confidence_edge_weight_cuts_self_reinforcement
#[test]
fn confidence_edge_weight_cuts_self_reinforcement() {
    let draft = koios::compile::edge_weight("draft");
    let medium = koios::compile::edge_weight("medium");
    let high = koios::compile::edge_weight("high");
    assert!(
        draft < medium && medium < high,
        "confidence must monotonically raise edge weight: {draft} {medium} {high}"
    );
    assert!(
        draft == 0.0,
        "draft pages must contribute no graph weight at all (§5.4)"
    );
}

/// test_ppr_drops_draft_edges
#[test]
fn ppr_drops_draft_edges() {
    let (v, conn) = seeded("ppr");
    // every doc starts at draft; the graph must therefore be weightless
    let ents = koios::compile::extract_entities(&blocks_of(&conn));
    let _ = koios::compile::write_entity_pages(&v, &ents, "2026-01-01");
    let graph = koios::retrieval::load_graph(&conn).unwrap();
    let total_w: f64 = graph.values().flat_map(|e| e.iter().map(|(_, w)| *w)).sum();
    assert!(
        total_w == 0.0,
        "draft-only graph must have zero total weight; got {total_w}"
    );
}

/// test_answer_write_back_is_draft_with_evidence
#[test]
fn answer_write_back_is_draft_with_evidence() {
    let (v, conn) = seeded("ans");
    let ev = koios::pipeline::retrieve_channel(&conn, "revenue", 5, None);
    assert!(!ev.is_empty());
    let p =
        koios::compile::write_answer_page(&v, "revenue?", "32 yuan", &ev, "2026-01-01").unwrap();
    let text = fs::read_to_string(&p).unwrap();
    assert!(text.contains("draft"), "answer page must start at draft");
    assert!(text.contains("[["), "answer page must carry citations");
}

/// test_slugify_is_stable
#[test]
fn slugify_is_stable() {
    let a = koios::compile::slugify("Hello World");
    let b = koios::compile::slugify("Hello World");
    assert_eq!(a, b, "slugify must be deterministic");
    assert!(!a.is_empty(), "slugify must not collapse to empty");
    // CJK must survive rather than being dropped
    let c = koios::compile::slugify("财务分析");
    assert!(!c.is_empty(), "CJK topic must still produce a slug");
}
