//! §15 index locking and §16 backup/restore.
//!
//! Both are driven through the real binary: the lock is only meaningful
//! between processes, and a restore utility is only trustworthy when
//! exercised end to end.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_koios"))
}

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-ops-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    p
}

fn seeded(name: &str) -> PathBuf {
    let v = vault(name);
    bin().arg("init").arg(&v).output().unwrap();
    fs::write(v.join("raw").join("a.md"), "# A\n\nbody text here.\n").unwrap();
    let idx = bin()
        .args(["index", "--allow-no-embedding"])
        .arg(&v)
        .output()
        .unwrap();
    assert!(idx.status.success(), "index failed: {idx:?}");
    v
}

#[test]
fn a_live_index_run_blocks_a_second_one() {
    let v = seeded("lock");
    // Hold the lock with a live PID, as a concurrent `koios index` would.
    let holder = std::process::Command::new("sleep")
        .arg("20")
        .spawn()
        .unwrap();
    fs::write(v.join(".index").join("index.lock"), holder.id().to_string()).unwrap();

    let out = bin()
        .args(["index", "--allow-no-embedding"])
        .arg(&v)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a second concurrent index must not proceed"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("already running"),
        "the error must say why: {err}"
    );
    drop(holder);
    fs::remove_file(v.join(".index").join("index.lock")).ok();
}

#[test]
fn a_stale_lock_does_not_wedge_the_vault() {
    let v = seeded("stale");
    // A PID that is certainly gone: the lock is left by a killed run.
    fs::write(v.join(".index").join("index.lock"), "999999").unwrap();
    let out = bin()
        .args(["index", "--allow-no-embedding"])
        .arg(&v)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a dead holder must be reclaimed, not block forever: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn backup_then_restore_returns_the_sources() {
    let v = seeded("backup");
    let dest = vault("backup-dest");
    fs::create_dir_all(&dest).unwrap();

    let out = bin()
        .args(["backup", "-p"])
        .arg(&v)
        .args(["-o"])
        .arg(&dest)
        .output()
        .unwrap();
    assert!(out.status.success(), "backup failed: {out:?}");

    let snap = fs::read_dir(&dest)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .expect("backup must create a snapshot directory");
    assert!(snap.join("raw").join("a.md").is_file());
    assert!(snap.join("BACKUP.txt").is_file());

    let restored = vault("restored");
    let out = bin()
        .arg("restore")
        .arg(&snap)
        .args(["-o"])
        .arg(&restored)
        .output()
        .unwrap();
    assert!(out.status.success(), "restore failed: {out:?}");
    assert_eq!(
        fs::read_to_string(restored.join("raw").join("a.md")).unwrap(),
        fs::read_to_string(v.join("raw").join("a.md")).unwrap(),
        "restored sources must match the original"
    );
}

#[test]
fn restore_refuses_to_overwrite_a_non_empty_directory() {
    let v = seeded("refuse");
    let dest = vault("refuse-dest");
    fs::create_dir_all(&dest).unwrap();
    let out = bin()
        .arg("restore")
        .arg(&v)
        .args(["-o"])
        .arg(&dest)
        .output()
        .unwrap();
    assert!(out.status.success(), "first restore should succeed");

    let out2 = bin()
        .arg("restore")
        .arg(&v)
        .args(["-o"])
        .arg(&dest)
        .output()
        .unwrap();
    assert!(
        !out2.status.success(),
        "a restore must not silently replace a populated vault"
    );
}
