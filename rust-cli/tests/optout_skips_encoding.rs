//! `--allow-no-embedding` must skip encoding, not just skip the check.
//!
//! Measured: with the model files present, indexing 600 blocks took 79s even
//! with the flag set, because `is_ready()` only asks whether the files exist
//! and the build ran regardless. The opt-out now gates the build too.
//! 0.17s -> 79s was the bug; 0.19s is the fix.

use std::fs;
use std::path::PathBuf;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-opt-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

#[test]
fn opt_out_leaves_no_vectors_behind() {
    let v = vault("optout");
    fs::write(v.join("raw").join("a.md"), "# A\n\nbody text here.\n").unwrap();
    koios::embed::set_allow_lexical(true);
    koios::cmd_index(&v).unwrap();

    // Even if a model happens to be installed, opting out must not produce
    // vectors -- otherwise the vault is half-semantic and behaves differently
    // from one indexed without the flag.
    let conn = koios::embed::ensure_vec_db(&v).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM vec_src", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "opting out must not encode anything: {n} rows");
    koios::embed::set_allow_lexical(false);
}
