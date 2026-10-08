//! `koios search` stdout must stay byte-identical to `koiosbase/cli.py`
//! cmd_search()'s three-line form — `[i] id / breadcrumb / snippet`.
//!
//! The Rust shell used to print `[doc_path] id` and drop the breadcrumb
//! entirely, so a hit could not be turned into a `[[wikilink]]` without
//! re-resolving it, and the two shells' stdout could not be diffed directly.
//! The Python suite tests only the Python side, so nothing caught the drift.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const DOC: &str =
    "---\ntitle: Report\n---\n# 财务分析\n## 负债分析\n负债合计 18 亿元，营收 32 亿元。\n";

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-search-fmt-{}-{}", name, std::process::id()));
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

/// The three-line form is what a caller needs to quote the block as a
/// `[[wikilink]]` without a second lookup, so assert the shape rather than
/// "the id appears somewhere in stdout".
#[test]
fn search_stdout_uses_the_python_three_line_form() {
    let p = vault("fmt");
    let bin = env!("CARGO_BIN_EXE_koios");

    let init = Command::new(bin).arg("init").arg(&p).output().unwrap();
    assert!(init.status.success(), "init failed: {init:?}");
    let idx = Command::new(bin).arg("index").arg(&p).output().unwrap();
    assert!(idx.status.success(), "index failed: {idx:?}");

    let out = Command::new(bin)
        .args(["search", "营收", "-p"])
        .arg(&p)
        .output()
        .unwrap();
    assert!(out.status.success(), "search failed: {out:?}");

    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty(), "search printed nothing");
    // The header line must be `[i] id` (old Rust printed `[doc_path] id`), and
    // every hit is three lines. Derived sources/ pages rank first because they
    // summarize the raw doc — the same order Python produces.
    assert_eq!(lines.len() % 3, 0, "stdout is not triples:\n{stdout}");
    for (n, triple) in lines.chunks(3).enumerate() {
        assert!(
            triple[0].starts_with(&format!("[{n}] ")) && triple[0].matches("] ").count() == 1,
            "line {} must be `[i] id` once — got {:?}\n{stdout}",
            n * 3,
            triple[0]
        );
        assert!(
            triple[1].starts_with("    ") && triple[1].contains(" > "),
            "line {} must be the indented breadcrumb — got {:?}\n{stdout}",
            n * 3 + 1,
            triple[1]
        );
        assert!(
            triple[2].starts_with("    负债合计"),
            "line {} must be the indented snippet — got {:?}\n{stdout}",
            n * 3 + 2,
            triple[2]
        );
    }
    // The block every caller needs must be present in the three-line form.
    assert!(
        stdout.contains("[1] r.md#财务分析/负债分析/1\n    Report > 财务分析 > 负债分析\n"),
        "missing the three-line form for the raw block:\n{stdout}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("] r.md] ")),
        "must not print `[doc_path] id` — got {stdout}"
    );

    let _ = fs::remove_dir_all(&p);
}

/// Both shells must agree on the empty-result sentinel: Python's cmd_search
/// prints `(no hits)`, so anything else breaks scripts diffing the two.
#[test]
fn search_with_no_hits_prints_the_shared_sentinel() {
    let p = vault("empty");
    let bin = env!("CARGO_BIN_EXE_koios");
    Command::new(bin).arg("init").arg(&p).output().unwrap();
    Command::new(bin).arg("index").arg(&p).output().unwrap();

    let out = Command::new(bin)
        .args(["search", "zzzzz-not-present", "-p"])
        .arg(&p)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "(no hits)\n");
    let _ = fs::remove_dir_all(&p);
}
