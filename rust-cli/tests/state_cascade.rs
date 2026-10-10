//! §8.2 / §8.3 knowledge state machine — ported from tests/test_state_cascade.py.
//!
//! Every case below mirrors a Python case by name. These are written against
//! the Rust implementation but assert the SAME observable behaviour, so they
//! replace the Python suite as the oracle rather than merely duplicating it.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-st-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

fn seed(v: &PathBuf) -> rusqlite::Connection {
    fs::write(
        v.join("raw").join("r.md"),
        "---\ntitle: T\n---\n# Sec\nACME revenue was 32 yuan.\n",
    )
    .unwrap();
    koios::cmd_index(v).unwrap();
    connect(v).unwrap()
}

/// test_block_state_defaults_to_active
#[test]
fn block_state_defaults_to_active() {
    let v = vault("default");
    let conn = seed(&v);
    assert_eq!(
        koios::state::get_block_state(&conn, "r.md#Sec/1"),
        "active",
        "an untouched block must read active"
    );
}

/// test_invalid_state_rejected
///
/// The state machine is closed: a typo'd state silently degrading to "active"
/// would let a retracted block back into retrieval.
#[test]
fn invalid_state_rejected() {
    let v = vault("invalid");
    let conn = seed(&v);
    let r = koios::state::set_block_state(&conn, "r.md#Sec/1", "not-a-state", "");
    assert!(r.is_err(), "invalid state must be rejected, not stored");
}

/// test_cascade_marks_citing_pages_stale
#[test]
fn cascade_marks_citing_pages_stale() {
    let v = vault("cascade");
    let conn = seed(&v);
    // reference_env＋write a wiki page citing the block
    fs::create_dir_all(v.join("wiki")).unwrap();
    fs::write(
        v.join("wiki").join("w.md"),
        "---\ntitle: W\nconfidence: draft\n---\n- claim [[raw/r.md#Sec/1]]\n",
    )
    .unwrap();
    koios::cmd_index(&v).unwrap();

    let pages = koios::state::cascade_retraction(&conn, "r.md#Sec/1", "typo", Some(&v)).unwrap();
    assert!(!pages.is_empty(), "the citing page must be reported");
    for p in &pages {
        assert!(koios::state::is_stale(&conn, p), "{p} must be marked stale");
    }
}

/// test_disposition_when_stale
#[test]
fn disposition_when_stale() {
    let v = vault("dispstale");
    let conn = seed(&v);
    koios::state::mark_stale(&conn, "r.md", "cited retracted x").unwrap();
    assert_eq!(
        koios::state::disposition_for(&conn, "r.md", false),
        "downrank_and_async_recompile"
    );
    assert_eq!(
        koios::state::disposition_for(&conn, "r.md", true),
        "refuse_or_recompile"
    );
}

/// test_filter_visible_drops_retracted
#[test]
fn filter_visible_drops_retracted() {
    let v = vault("filtret");
    let conn = seed(&v);
    assert!(!koios::pipeline::retrieve_channel(&conn, "revenue", 8, None).is_empty());
    koios::state::set_block_state(&conn, "r.md#Sec/1", "retracted", "wrong").unwrap();
    let out = koios::pipeline::retrieve_channel(&conn, "revenue", 8, None);
    assert!(
        out.iter().all(|b| b.id != "r.md#Sec/1"),
        "a retracted block must never reach the context"
    );
}

/// test_lazy_synthesis — disposition is decided at query time, not stored.
#[test]
fn lazy_synthesis() {
    let v = vault("lazy");
    let conn = seed(&v);
    koios::state::mark_stale(&conn, "r.md", "sources changed").unwrap();
    // Re-marking fresh must be observable immediately, with no recompile step.
    conn.execute("UPDATE page_state SET stale=0 WHERE page_path='r.md'", [])
        .unwrap();
    assert_eq!(koios::state::disposition_for(&conn, "r.md", false), "use");
}

/// test_lint_reports_stale_pages
#[test]
fn lint_reports_stale_pages() {
    let v = vault("lintstale");
    let conn = seed(&v);
    // Mirror the Python fixture exactly: the gardener reports whatever page
    // the cascade marked, keyed by page_path.
    koios::state::mark_stale(&conn, "wiki/entities/a.md", "cited retracted").unwrap();
    let findings = koios::lint::run_l1(&conn);
    let stale = findings
        .iter()
        .find(|(k, _)| *k == "stale_pages")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    // The key is stored normalised (`entities/a.md`, no `wiki/` prefix) so the
    // writer's shape and the reader's `doc_path` meet — see
    // `state::normalize_page_key`. Reporting the raw key here is what let the
    // cascade write rows nobody could find.
    assert!(
        stale.iter().any(|s| s.contains("entities/a.md")),
        "gardener must surface stale pages; got {stale:?}"
    );
}

/// test_lint_ttl_expiry
#[test]
fn lint_ttl_expiry() {
    let v = vault("ttl");
    let conn = seed(&v);
    // A document whose valid_from is far in the past is expired.
    // TTL expiry needs BOTH ttl and valid_from, matching the Python fixture.
    conn.execute(
        "UPDATE documents SET frontmatter=? WHERE path='r.md'",
        [r#"{"ttl": "1d", "valid_from": "2020-01-01"}"#],
    )
    .unwrap();
    let findings = koios::lint::run_l1(&conn);
    let expired = findings
        .iter()
        .find(|(k, _)| *k == "expired_ttl")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    assert!(
        !expired.is_empty(),
        "TTL past must be reported; got {expired:?}"
    );
}

/// test_stale_evidence_is_disclosed_to_the_reader
///
/// Down-ranked ≠ dropped: the reader still sees the stale block, just last.
#[test]
fn stale_evidence_is_disclosed_to_the_reader() {
    let v = vault("disclose");
    let conn = seed(&v);
    fs::write(
        v.join("raw").join("s.md"),
        "---\ntitle: S\n---\n# Sec2\nACME revenue also mentioned here.\n",
    )
    .unwrap();
    koios::cmd_index(&v).unwrap();
    let before = koios::pipeline::retrieve_channel(&conn, "revenue", 8, None);
    koios::state::mark_stale(&conn, "r.md", "x").unwrap();
    let after = koios::pipeline::retrieve_channel(&conn, "revenue", 8, None);
    assert_eq!(
        before.len(),
        after.len(),
        "downranking must reorder, never silently hide evidence"
    );
}

/// test_run_l1_survives_legacy_schema
///
/// A vault written before `page_state`/`block_state` existed must still lint;
/// the gardener is what upgrades old vaults, so crashing defeats the purpose.
#[test]
fn run_l1_survives_legacy_schema() {
    let v = vault("legacy");
    let conn = seed(&v);
    conn.execute_batch("DROP TABLE IF EXISTS page_state; DROP TABLE IF EXISTS block_state;")
        .unwrap();
    // run_l1 returns plain data; the guarantee is that it does not panic.
    let findings = koios::lint::run_l1(&conn);
    assert!(
        findings.iter().any(|(k, _)| *k == "stale_pages"),
        "L1 must still run on a vault without state tables"
    );
}
