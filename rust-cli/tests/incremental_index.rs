//! Incremental reindex, lock ordering, and WAL — the production-deployment
//! behaviours reported against 0.11.7 from a 25k-block vault.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-incr-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    fs::write(
        p.join("raw").join("a.md"),
        "---\ntitle: A\n---\n\n# A\n\n## 节\n内容甲。\n",
    )
    .unwrap();
    fs::write(
        p.join("raw").join("b.md"),
        "---\ntitle: B\n---\n\n# B\n\n## 节\n内容乙，引用 [[A]]。\n",
    )
    .unwrap();
    koios::embed::set_allow_lexical(true);
    p
}

fn block_count(v: &std::path::Path, doc: &str) -> i64 {
    connect(v)
        .unwrap()
        .query_row("SELECT count(*) FROM blocks WHERE doc_path=?", [doc], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn unchanged_files_are_skipped_and_changes_are_not() {
    let v = vault("skip");
    koios::cmd_index(&v).unwrap();
    let before = block_count(&v, "a.md");
    assert!(before > 0);

    // Corrupt the DB directly: delete a block row of a.md. If the second run
    // re-parsed the file, the row would come back; with the skip it must not.
    connect(&v)
        .unwrap()
        .execute("DELETE FROM blocks WHERE doc_path='a.md'", [])
        .unwrap();
    koios::cmd_index(&v).unwrap();
    assert_eq!(
        block_count(&v, "a.md"),
        0,
        "unchanged file must be skipped (mtime+size token)"
    );

    // Touching the file (mtime changes) must force re-parse.
    let p = v.join("raw").join("a.md");
    let t = fs::read_to_string(&p).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(&p, t).unwrap();
    koios::cmd_index(&v).unwrap();
    assert_eq!(
        block_count(&v, "a.md"),
        before,
        "mtime change must force re-parse"
    );
}

#[test]
fn links_do_not_accumulate_across_reruns() {
    let v = vault("links");
    koios::cmd_index(&v).unwrap();
    let n1: i64 = connect(&v)
        .unwrap()
        .query_row("SELECT count(*) FROM links", [], |r| r.get(0))
        .unwrap();
    koios::cmd_index(&v).unwrap();
    koios::cmd_index(&v).unwrap();
    let n2: i64 = connect(&v)
        .unwrap()
        .query_row("SELECT count(*) FROM links", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n1, n2, "links must not accumulate across reruns");
    assert!(n1 > 0, "wikilink + structural edges must exist");
}

#[test]
fn full_forces_reparse_despite_unchanged_files() {
    let v = vault("full");
    koios::cmd_index(&v).unwrap();
    connect(&v)
        .unwrap()
        .execute("DELETE FROM blocks WHERE doc_path='a.md'", [])
        .unwrap();
    koios::cmd_index_with(&v, true).unwrap();
    assert!(
        block_count(&v, "a.md") > 0,
        "--full must bypass the incremental skip"
    );
}

#[test]
fn full_does_not_drop_vectors_before_winning_the_lock() {
    let v = vault("lockfull");
    koios::cmd_index(&v).unwrap();
    // Seed a marker row in vec_meta; --full drops vec tables entirely, so its
    // survival proves the drop did not run.
    let conn = koios::embed::ensure_vec_db(&v).unwrap();
    conn.execute_batch(koios::embed::VEC_SCHEMA).unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO vec_meta(key,value) VALUES('marker','alive')",
        [],
    )
    .unwrap();
    drop(conn);

    let held = koios::IndexLock::acquire(&v).unwrap();
    // A second index must fail on the lock -- and must NOT have reset vectors
    // on its way to that failure.
    let r = koios::cmd_index_with(&v, true);
    assert!(r.is_err(), "concurrent index must fail on the lock");
    let conn = koios::embed::ensure_vec_db(&v).unwrap();
    let alive: String = conn
        .query_row("SELECT value FROM vec_meta WHERE key='marker'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(alive, "alive", "lock loser must not destroy stored vectors");
    drop(held);
}

#[test]
fn wal_is_on_so_readers_do_not_block_behind_a_writer() {
    let v = vault("wal");
    let conn = connect(&v).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal", "tree.db must run in WAL mode");
}
