//! A deleted raw document must actually leave the vault.
//!
//! Re-index refreshed whatever was present and never removed anything: with
//! `raw/x.md` gone, its blocks, sections, `documents` row and the generated
//! `wiki/sources/x.md` all stayed behind and kept answering queries. The only
//! remedy was deleting `.index/` whole, which also throws away every derived
//! page -- so the corpus could only grow.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-del-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

fn index(v: &std::path::Path) {
    koios::embed::set_allow_lexical(true);
    koios::cmd_index(v).unwrap();
}

fn blocks_mentioning(conn: &rusqlite::Connection, needle: &str) -> usize {
    let mut stmt = conn
        .prepare("SELECT COUNT(*) FROM blocks WHERE doc_path LIKE ?1")
        .unwrap();
    stmt.query_row([format!("%{needle}%")], |r| r.get::<_, i64>(0))
        .unwrap() as usize
}

#[test]
fn deleting_a_raw_document_reclaims_its_blocks() {
    let v = vault("blocks");
    fs::write(
        v.join("raw").join("keep.md"),
        "# Keep\n\nalpha revenue grew.\n",
    )
    .unwrap();
    fs::write(
        v.join("raw").join("drop.md"),
        "# Drop\n\nbeta revenue fell.\n",
    )
    .unwrap();
    index(&v);
    let conn = connect(&v).unwrap();
    assert!(
        blocks_mentioning(&conn, "drop.md") > 0,
        "fixture must index it"
    );

    fs::remove_file(v.join("raw").join("drop.md")).unwrap();
    index(&v);
    let conn = connect(&v).unwrap();
    assert_eq!(
        blocks_mentioning(&conn, "drop.md"),
        0,
        "a deleted document must not leave blocks behind"
    );
    assert!(
        blocks_mentioning(&conn, "keep.md") > 0,
        "the surviving document must be untouched"
    );
}

#[test]
fn deleting_a_raw_document_retires_its_derived_page() {
    let v = vault("derived");
    fs::write(v.join("raw").join("drop.md"), "# D\n\nbody text here.\n").unwrap();
    index(&v);
    let generated = v.join("wiki").join("sources").join("drop.md");
    assert!(generated.is_file(), "index must generate sources/drop.md");

    fs::remove_file(v.join("raw").join("drop.md")).unwrap();
    index(&v);
    assert!(
        !generated.is_file(),
        "the derived page of a deleted source must be retired"
    );
    let conn = connect(&v).unwrap();
    assert_eq!(
        blocks_mentioning(&conn, "drop.md"),
        0,
        "derived blocks must be purged too, or the document keeps answering"
    );
}

#[test]
fn a_first_index_purges_nothing() {
    let v = vault("first");
    fs::write(v.join("raw").join("a.md"), "# A\n\nsomething.\n").unwrap();
    index(&v);
    let conn = connect(&v).unwrap();
    // No fingerprint history on a cold vault; diffing there would flag every
    // document as deleted on its first appearance.
    assert!(blocks_mentioning(&conn, "a.md") > 0);
}
