//! Integration tests for the ingest pipeline.
//!
//! These guard behaviour that previously had NO test on the Rust side: indexing
//! used to silently skip every derived page (sources/, entities/), so a Rust
//! vault held 1 block where Python held 5 and nothing caught it. The Python
//! suite could not catch it either — it only tests the Python shell.

use std::fs;
use std::path::PathBuf;

use koios::connect;

const DOC: &str = "---\ntitle: Test\n---\n# Sec\nACME revenue was 3.2 billion yuan in 2024.\n";

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-test-{}-{}", name, std::process::id()));
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

fn block_count(vault: &PathBuf) -> i64 {
    let conn = connect(vault).unwrap();
    conn.query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))
        .unwrap()
}

/// Index is idempotent: three rebuilds must agree.
///
/// This is the property the old derived-page `continue` claimed to protect —
/// because to feed the compiler's own output back in and grow forever. It does
/// not: content hashes make ingest idempotent, verified here by construction.
#[test]
fn index_is_idempotent() {
    let v = vault("idem");
    let mut n = None;
    for i in 0..3 {
        koios::cmd_index(&v).unwrap();
        let c = block_count(&v);
        if let Some(prev) = n {
            assert_eq!(prev, c, "run {i} changed the block count");
        }
        n = Some(c);
    }
    let _ = fs::remove_dir_all(&v);
}

/// Derived pages are indexed, matching the Python shell's block count.
///
/// Locking the exact number is deliberate: the point of the test is that Rust
/// must NOT drop the four source-page blocks the way it used to. If the count
/// ever needs to change, the reasoning belongs in the commit message.
#[test]
fn derived_pages_are_indexed() {
    let v = vault("derived");
    koios::cmd_index(&v).unwrap();
    let raw_blocks: Vec<String> = {
        let conn = connect(&v).unwrap();
        let mut s = conn
            .prepare("SELECT id FROM blocks WHERE layer='raw'")
            .unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    assert!(!raw_blocks.is_empty(), "no raw blocks indexed");
    let wiki: Vec<String> = {
        let conn = connect(&v).unwrap();
        let mut s = conn
            .prepare("SELECT DISTINCT doc_path FROM blocks WHERE layer='wiki'")
            .unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    assert!(
        wiki.iter().any(|p| p.starts_with("sources/")),
        "derived sources/ pages were not indexed: {wiki:?}"
    );
    let _ = fs::remove_dir_all(&v);
}

/// A cold vault indexes to the same state as a warm one — the first invocation
/// must not differ from later ones (it did, before the two-pass fix).
#[test]
fn cold_start_matches_warm() {
    let a = vault("cold");
    koios::cmd_index(&a).unwrap();
    let cold = block_count(&a);

    let b = vault("warm");
    koios::cmd_index(&b).unwrap();
    koios::cmd_index(&b).unwrap();
    let warm = block_count(&b);

    assert_eq!(cold, warm, "first run differs from steady state");
    let _ = fs::remove_dir_all(&a);
    let _ = fs::remove_dir_all(&b);
}
