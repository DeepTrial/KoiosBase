//! §8.2 / §8.3 knowledge state machine — ported from tests/test_state_cascade.py.
//!
//! Every case below mirrors a Python case by name. These are written against
//! the Rust implementation but assert the SAME observable behaviour, so they
//! replace the Python suite as the oracle rather than merely duplicating it.

use std::fs;
use std::path::{Path, PathBuf};

use koios::connect;

/// These fixtures deliberately carry no embedding model. That is now a hard
/// error (§5.2.1 is fail-closed), so every fixture opts in explicitly rather
/// than relying on the old "warn and continue" behaviour.
fn lexical() {
    koios::embed::set_allow_lexical(true);
}

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-st-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

fn seed(v: &Path) -> rusqlite::Connection {
    fs::write(
        v.join("raw").join("r.md"),
        "---\ntitle: T\n---\n# Sec\nACME revenue was 32 yuan.\n",
    )
    .unwrap();
    // Fixture carries no model; fail-closed indexing requires the opt-in.
    koios::embed::set_allow_lexical(true);
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
    (lexical(), koios::cmd_index(&v)).1.unwrap();

    let pages = koios::state::cascade_retraction(&conn, "r.md#Sec/1", "typo", Some(&v)).unwrap();
    assert!(!pages.is_empty(), "the citing page must be reported");
    for p in &pages {
        assert!(koios::state::is_stale(&conn, p), "{p} must be marked stale");
    }
}

/// P1/P5: a human retraction is a Markdown fact, not disposable index state.
/// Exercise both an ordinary rebuild and recovery from a deleted .index, and
/// check the blast radius as well as the block itself.
#[test]
fn retraction_survives_reindex_and_cold_rebuild() {
    let v = vault("persistent-retraction");
    let conn = seed(&v);
    fs::write(
        v.join("wiki").join("w.md"),
        "---\ntitle: W\nconfidence: draft\n---\n- claim [[raw/r.md#Sec/1]]\n",
    )
    .unwrap();
    (lexical(), koios::cmd_index(&v)).1.unwrap();

    let block_id = "r.md#Sec/1";
    koios::state::cascade_retraction(&conn, block_id, "wrong figure", Some(&v)).unwrap();
    let ledger = fs::read_to_string(v.join("raw/.koios/state.md")).unwrap();
    assert!(ledger.contains(block_id), "retraction must be in Markdown");
    assert!(ledger.contains("wrong figure"), "the audit reason must survive");
    // A document-level active declaration must not silently undo a more
    // specific human decision about one block.
    let raw = fs::read_to_string(v.join("raw/r.md")).unwrap();
    fs::write(v.join("raw/r.md"), raw.replace("title: T", "title: T\nstate: active")).unwrap();

    drop(conn);
    for cold_rebuild in [false, true] {
        if cold_rebuild {
            fs::remove_dir_all(v.join(".index")).unwrap();
        }
        (lexical(), koios::cmd_index(&v)).1.unwrap();
        let conn = koios::connect(&v).unwrap();
        assert_eq!(koios::state::get_block_state(&conn, block_id), "retracted");
        let reason: String = conn
            .query_row(
                "SELECT reason FROM block_state WHERE block_id=?",
                [block_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "wrong figure");
        assert!(
            koios::state::is_stale(&conn, "w.md"),
            "a cold rebuild must rederive the citing page's staleness"
        );
        let visible = koios::pipeline::retrieve_channel(&conn, "revenue", 8, None);
        assert!(visible.iter().all(|b| b.id != block_id));
        let indexed_ledger: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM documents WHERE path LIKE '.koios/%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexed_ledger, 0, "the state ledger is metadata, not corpus");
        if cold_rebuild {
            let states = vec!["retracted".to_string()];
            let history = koios::pipeline::retrieve_all(
                &v,
                &conn,
                "revenue",
                8,
                None,
                &koios::pipeline::Query {
                    include_states: &states,
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(history.iter().any(|b| b.id == block_id));
        }
    }
}

#[test]
fn malformed_state_ledger_fails_closed() {
    let v = vault("malformed-state-ledger");
    let conn = seed(&v);
    koios::state::cascade_retraction(&conn, "r.md#Sec/1", "wrong", Some(&v)).unwrap();
    drop(conn);
    fs::write(v.join("raw/.koios/state.md"), "# corrupt state\n").unwrap();
    let err = koios::cmd_index(&v).unwrap_err().to_string();
    assert!(
        err.contains("no JSON state block"),
        "a corrupt source-of-truth ledger must not silently reactivate blocks: {err}"
    );
}

#[test]
fn retraction_does_not_stick_to_different_content_at_the_same_id() {
    let v = vault("content-bound-retraction");
    let conn = seed(&v);
    koios::state::cascade_retraction(&conn, "r.md#Sec/1", "wrong", Some(&v)).unwrap();
    drop(conn);
    fs::write(
        v.join("raw/r.md"),
        "---\ntitle: T\n---\n# Sec\nACME revenue was 99 yuan.\n",
    )
    .unwrap();
    (lexical(), koios::cmd_index(&v)).1.unwrap();
    let conn = koios::connect(&v).unwrap();
    assert_eq!(
        koios::state::get_block_state(&conn, "r.md#Sec/1"),
        "active",
        "a verdict on the old assertion must not retract new content"
    );
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
    (lexical(), koios::cmd_index(&v)).1.unwrap();
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
