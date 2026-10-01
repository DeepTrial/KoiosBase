//! Cross-shell parity for §8.3 disposition and §4.4 init contract.
//!
//! Both behaviours were MISSING from the Rust shell:
//!   * §8.3 apply_disposition — Python's query/pipeline.py retrieve() applies it
//!     after filter_visible, so `koios retract` changed nothing here: a
//!     retracted page's evidence kept answering questions.
//!   * §4.4 AGENTS.md — init wrote a one-line stub instead of the four-rule
//!     maintenance contract, so a Rust-initialised vault silently lost three
//!     rules the Python side writes.
//! The Python suite tests only the Python shell, so neither gap could be caught
//! from there.

use std::fs;
use std::path::PathBuf;

use koios::connect;

const DOC: &str = "---\ntitle: Test\n---\n# Sec\nACME revenue was 3.2 billion yuan.\n";

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-par-{}-{}", name, std::process::id()));
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
    fs::write(p.join("raw").join("r.md"), DOC).unwrap();
    p
}

/// §8.3: evidence from a page whose sources moved on must sink behind fresh
/// evidence, never be presented as equally current.
///
/// `disposition_for` alone is not enough — it is a lookup. This asserts the
/// ORDERING on the actual read path (`retrieve_channel`), which is what both
/// retrieval and Grader escalation consume.
#[test]
fn stale_page_is_downranked_on_the_read_path() {
    let v = vault("stale");
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();

    let before = koios::pipeline::retrieve_channel(&conn, "revenue", 8, None);
    assert!(!before.is_empty(), "baseline: retrieve must find the block");

    // Mark the source page stale, exactly as `koios retract` does to citing pages.
    koios::state::mark_stale(&conn, "r.md", "cited retracted something").unwrap();

    let after = koios::pipeline::retrieve_channel(&conn, "revenue", 8, None);
    assert_eq!(
        before.len(),
        after.len(),
        "downrank reorders, it must NOT silently drop evidence"
    );
    // The stale doc's blocks must now come last.
    let first_is_fresh = after.first().map(|b| b.doc_path != "r.md").unwrap_or(true);
    let last_is_stale = after.last().map(|b| b.doc_path == "r.md").unwrap_or(false);
    assert!(
        first_is_fresh && last_is_stale,
        "stale page must sink to the end; got {:?}",
        after.iter().map(|b| b.doc_path.clone()).collect::<Vec<_>>()
    );
}

/// §8.3: a retracted page is `hard_filter` — it vanishes, it does not merely sink.
#[test]
fn retracted_page_is_hard_filtered() {
    let v = vault("retract");
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();
    koios::state::mark_stale(&conn, "r.md", "cited retracted x").unwrap();
    conn.execute(
        "UPDATE page_state SET state='retracted' WHERE page_path='r.md'",
        [],
    )
    .unwrap();

    let out = koios::pipeline::retrieve_channel(&conn, "revenue", 8, None);
    assert!(
        out.iter().all(|b| b.doc_path != "r.md"),
        "retracted page must never reach the context; got {:?}",
        out.iter().map(|b| b.doc_path.clone()).collect::<Vec<_>>()
    );
}

/// Cross-check against koiosbase/state/model.py on the same inputs. Python's
/// `_disposition` returned these values; a change here means the two shells
/// disagree about what "stale" means at query time.
#[test]
fn disposition_table_matches_python() {
    let v = vault("disp");
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();

    // no row -> use
    assert_eq!(
        koios::state::disposition_for(&conn, "missing.md", false),
        "use"
    );

    koios::state::mark_stale(&conn, "r.md", "x").unwrap();
    assert_eq!(
        koios::state::disposition_for(&conn, "r.md", false),
        "downrank_and_async_recompile"
    );
    assert_eq!(
        koios::state::disposition_for(&conn, "r.md", true),
        "refuse_or_recompile"
    );

    conn.execute(
        "UPDATE page_state SET state='retracted' WHERE page_path='r.md'",
        [],
    )
    .unwrap();
    assert_eq!(
        koios::state::disposition_for(&conn, "r.md", false),
        "hard_filter"
    );

    conn.execute(
        "UPDATE page_state SET state='superseded', stale=0 WHERE page_path='r.md'",
        [],
    )
    .unwrap();
    assert_eq!(
        koios::state::disposition_for(&conn, "r.md", false),
        "filtered_unless_explicit_history"
    );
}

/// §4.4: AGENTS.md is the maintenance contract written INTO the vault. A short
/// stub silently drops three rules, so assert the exact text rather than "file
/// exists" — the old test-shaped check would have passed on the stub.
#[test]
fn init_writes_the_full_maintenance_contract() {
    let p = std::env::temp_dir().join(format!("koios-init-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();

    koios::init_vault(&p).unwrap();

    let text = fs::read_to_string(p.join("AGENTS.md")).unwrap();
    for rule in [
        "[[wikilink]]",
        "confidence: draft",
        "cross-family verification",
        "koios index <vault>",
    ] {
        assert!(
            text.contains(rule),
            "AGENTS.md is missing rule {rule:?}:\n{text}"
        );
    }
    // Python marks the derived dir ignored too; without it a built vault looks
    // dirty to git.
    assert_eq!(
        fs::read_to_string(p.join(".index/.gitignore")).unwrap(),
        "*\n"
    );
}
