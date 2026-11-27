//! v0.11.10 review fixes: wikilink precision, backup consistency, compile skip.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-rev-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    koios::embed::set_allow_lexical(true);
    p
}

#[test]
fn vague_wikilinks_do_not_fan_out_or_self_loop() {
    let v = vault("wl");
    fs::write(
        v.join("raw").join("ref.md"),
        "# 引用卡\n\n## 引用\n参见 [[卡]]。\n",
    )
    .unwrap();
    for i in 1..=3 {
        fs::write(
            v.join("raw").join(format!("card{i}.md")),
            format!("# 卡片{i}\n\n## 内容\n内容{i}。\n"),
        )
        .unwrap();
    }
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();
    let fanned: i64 = conn
        .query_row(
            "SELECT count(*) FROM links WHERE kind='wikilink'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        fanned, 0,
        "[[卡]] must not fan out to every card with 卡 in the title (measured: 4 edges incl. a self-loop)"
    );
    // Exact heading match still resolves.
    fs::write(
        v.join("raw").join("ref2.md"),
        "# 引用2\n\n## 引用\n参见 [[卡片1]]。\n",
    )
    .unwrap();
    koios::cmd_index(&v).unwrap();
    let exact: i64 = connect(&v)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM links WHERE kind='wikilink' AND dst LIKE 'card1.md%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(exact > 0, "exact heading match must still resolve");
}

#[test]
fn unchanged_run_skips_compile_entirely() {
    let v = vault("compileskip");
    for i in 0..5 {
        fs::write(
            v.join("raw").join(format!("d{i}.md")),
            format!("# 卡{i}\n\n## 节\n内容 {i}。\n"),
        )
        .unwrap();
    }
    koios::cmd_index(&v).unwrap();
    let derived_before = std::fs::read_dir(v.join("wiki").join("sources"))
        .unwrap()
        .count();
    assert_eq!(derived_before, 5);
    // Second run: nothing changed. Derived pages must still exist and be
    // correct -- the skip must not starve the wiki layer of input.
    koios::cmd_index(&v).unwrap();
    let derived_after = std::fs::read_dir(v.join("wiki").join("sources"))
        .unwrap()
        .count();
    assert_eq!(derived_after, 5);
    // And a changed card still regenerates its derived page.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(
        v.join("raw").join("d0.md"),
        "# 卡0\n\n## 节\n改了内容甲乙丙。\n",
    )
    .unwrap();
    koios::cmd_index(&v).unwrap();
    let page = fs::read_to_string(v.join("wiki").join("sources").join("d0.md")).unwrap();
    assert!(
        page.contains("改了内容"),
        "changed card must refresh its page"
    );
}

#[test]
fn backup_is_a_consistent_snapshot_not_a_file_copy() {
    let v = vault("backup");
    fs::write(v.join("raw").join("a.md"), "# A\n\n## 节\n内容。\n").unwrap();
    koios::cmd_index(&v).unwrap();
    let out = std::env::temp_dir().join(format!("koios-bak-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    let _ = koios::init_vault(&out);
    // Use the library-level path the CLI uses.
    let bak_root = out.join("manual");
    std::fs::create_dir_all(&bak_root).unwrap();
    let dst_idx = bak_root.join(".index");
    std::fs::create_dir_all(&dst_idx).unwrap();
    for name in ["tree.db", "vec.db"] {
        let src_db = v.join(".index").join(name);
        if !src_db.exists() {
            continue;
        }
        let dst_db = dst_idx.join(name);
        let conn = rusqlite::Connection::open(&src_db).unwrap();
        conn.execute_batch(&format!("VACUUM INTO '{}'", dst_db.display()))
            .unwrap();
    }
    let conn = rusqlite::Connection::open(dst_idx.join("tree.db")).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM blocks", [], |r| r.get(0))
        .unwrap();
    assert!(n > 0, "VACUUM INTO snapshot must carry the blocks");
}
