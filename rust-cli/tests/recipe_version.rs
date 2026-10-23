//! Upgrade safety: a vector index must know which recipe built it, `--full`
//! must actually rebuild it, and encoding failures must not exit 0.

use std::fs;
use std::path::{Path, PathBuf};

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-rec-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    p
}

fn meta_of(v: &Path) -> std::collections::HashMap<String, String> {
    let conn = koios::embed::ensure_vec_db(v).unwrap();
    let mut out = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM vec_meta") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1).unwrap_or_default(),
            ))
        }) {
            for r in rows.flatten() {
                out.insert(r.0, r.1);
            }
        }
    }
    out
}

/// #1 — the recipe is part of the index identity. Changing how text is
/// assembled invalidates every vector while leaving model_id and dim
/// untouched, which is precisely the case the old check could not see.
#[test]
fn index_records_its_embedding_recipe() {
    let v = vault("recipe");
    let conn = koios::embed::ensure_vec_db(&v).unwrap();
    // Simulate an index written by an older build.
    koios::embed::stamp_identity_for_test(&conn).unwrap();
    let meta = meta_of(&v);
    assert_eq!(
        meta.get("embed_recipe").map(|s| s.as_str()),
        Some(koios::embed::EMBED_RECIPE),
        "recipe must be stamped into vec_meta"
    );
}

/// #1 — and a different recipe must be refused, not silently mixed.
#[test]
fn a_stale_recipe_is_rejected() {
    let v = vault("stalerecipe");
    let conn = koios::embed::ensure_vec_db(&v).unwrap();
    koios::embed::stamp_identity_for_test(&conn).unwrap();
    conn.execute(
        "UPDATE vec_meta SET value='space-joined:v1' WHERE key='embed_recipe'",
        [],
    )
    .unwrap();
    let err = koios::embed::check_identity_for_test(&conn).unwrap_err();
    assert!(
        err.contains("embed_recipe"),
        "a stale recipe must name itself: {err}"
    );
}

/// #2 — `--full` must clear stored vectors, or "full rebuild" leaves every
/// old vector in place under the guise of being complete.
#[test]
fn full_reset_clears_stored_vectors() {
    let v = vault("reset");
    let conn = koios::embed::ensure_vec_db(&v).unwrap();
    conn.execute(
        "INSERT INTO vec_src (block_id, hash) VALUES ('a#1','deadbeef')",
        [],
    )
    .unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM vec_src", [], |r| r.get(0))
        .unwrap();
    assert!(before > 0, "fixture must have a stored vector");

    koios::embed::reset_vectors(&v).unwrap();
    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM vec_src", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, 0, "--full must drop every stored vector");
}
