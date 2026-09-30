//! ACL regression locks (§9.2) — the Rust side lagged the Python fixes.
//!
//! Two real leaks were found here by measurement, not inspection:
//!   1. anonymous Studio brief contained a restricted HR figure;
//!   2. `koios search` bypassed ACL entirely (raw FTS, no filter).
//! The "entitled caller sees it" assertions matter as much as the leak ones —
//! a filter that drops everything also looks clean.

use std::fs;
use std::path::PathBuf;

const PUBLIC: &str = "---\ntitle: 公开\n---\n# 概览\n营收 32 亿元。\n";
const SECRET: &str = "---\ntitle: 机密\nacl: [finance-team]\n---\n# 薪酬\n高管薪酬 500 万。\n";

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-acl-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    for d in [
        "raw",
        "wiki/sources",
        "wiki/entities",
        "wiki/synthesis",
        ".index",
    ] {
        fs::create_dir_all(p.join(d)).unwrap();
    }
    fs::write(p.join("raw").join("open.md"), PUBLIC).unwrap();
    fs::write(p.join("raw").join("secret.md"), SECRET).unwrap();
    koios::cmd_index(&p).unwrap();
    p
}

fn groups(g: &[&str]) -> Vec<String> {
    g.iter().map(|x| x.to_string()).collect()
}

#[test]
fn grants_are_read_from_frontmatter() {
    let v = vault("grants");
    let conn = koios::connect(&v).unwrap();
    assert_eq!(koios::acl::grants_for(&conn, "open.md"), None);
    assert_eq!(
        koios::acl::grants_for(&conn, "secret.md"),
        Some(groups(&["finance-team"]))
    );
    // Derived pages inherit the ORIGIN's grant — a compiled page declares no
    // acl of its own, which is how restricted prose reached anonymous exports.
    assert_eq!(
        koios::acl::grants_for(&conn, "sources/secret.md"),
        Some(groups(&["finance-team"])),
        "derived page must inherit its origin's acl"
    );
    let _ = fs::remove_dir_all(&v);
}

#[test]
fn search_respects_principal() {
    let v = vault("search");
    let conn = koios::connect(&v).unwrap();
    let anon = koios::pipeline::retrieve_channel(&conn, "薪酬", 8, None);
    assert!(
        anon.iter().all(|b| !b.raw.contains("500 万")),
        "anonymous retrieved restricted prose"
    );
    let fin = koios::pipeline::retrieve_channel(&conn, "薪酬", 8, Some(&groups(&["finance-team"])));
    assert!(
        fin.iter().any(|b| b.raw.contains("500 万")),
        "entitled caller saw nothing"
    );
    let hr = koios::pipeline::retrieve_channel(&conn, "薪酬", 8, Some(&groups(&["hr"])));
    assert!(
        hr.iter().all(|b| !b.raw.contains("500 万")),
        "wrong group leaked restricted prose"
    );
    let _ = fs::remove_dir_all(&v);
}

#[test]
fn studio_export_respects_principal() {
    let v = vault("studio");
    let conn = koios::connect(&v).unwrap();

    let anon_path = koios::studio::studio_brief(&conn, &v, "薪酬", 8, None).unwrap();
    let anon = fs::read_to_string(&anon_path).unwrap();
    assert!(
        !anon.contains("500 万"),
        "anonymous export leaked the figure"
    );

    let fin_path =
        koios::studio::studio_brief(&conn, &v, "薪酬", 8, Some(&groups(&["finance-team"])))
            .unwrap();
    let fin = fs::read_to_string(&fin_path).unwrap();
    assert!(fin.contains("500 万"), "entitled caller saw nothing");
    let _ = fs::remove_dir_all(&v);
}

#[test]
fn mindmap_hides_restricted_titles() {
    let v = vault("mindmap");
    let conn = koios::connect(&v).unwrap();
    let p = koios::studio::studio_mindmap(&conn, &v, "全库", None).unwrap();
    let txt = fs::read_to_string(&p).unwrap();
    // A section TITLE is content: 「薪酬」 alone discloses pay data exists.
    assert!(!txt.contains("薪酬"), "restricted title leaked: {txt}");
    let _ = fs::remove_dir_all(&v);
}
