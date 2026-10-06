//! Two parity gaps found by diffing the two shells' stdout, not by reading the
//! code — both lived in the INDEXED form, so every downstream command silently
//! disagreed while unit tests on the helper functions stayed green.
//!
//! 1. `index_file` joined a block's lines with " " instead of "\n". A
//!    multi-line list became one line, so lint's line-oriented
//!    unsourced-assertion check reported 1 finding where Python reported 3,
//!    and every derived page's `blocks.raw` differed.
//! 2. `check_orphan_pages` looked only for an outbound `[[`, so every hub page
//!    — cited by many, citing none — was reported as orphan garbage.

use std::fs;
use std::path::PathBuf;

use koios::connect;

const RAW: &str = "---\ntitle: Report\n---\n# 财务分析\n## 负债分析\n负债合计 18 亿元，营收 32 亿元。\n## 现金流\n经营性现金流为正。\n";

// The derived sources/ page renders each question on its own "- " line, so the
// indexed block must be multi-line for lint to see three separate assertions.
const WIKI_LIST: &str = "---\ntitle: Hub\ntype: concept\nconfidence: draft\n---\n# Hub\n\n- 关于「财务分析」，本文档有哪些说明？ (report.md#财务分析)\n- 关于「负债分析」，本文档有哪些说明？ (report.md#财务分析/负债分析)\n- 关于「现金流」，本文档有哪些说明？ (report.md#财务分析/现金流)\n";

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-lint-par-{}-{}", name, std::process::id()));
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
    p
}

/// A block whose lines were joined with " " would be one line here; Python
/// keeps them separate, and lint counts findings per line.
#[test]
fn indexed_blocks_keep_their_interior_newlines() {
    let p = vault("nl");
    fs::write(p.join("raw").join("report.md"), RAW).unwrap();
    let conn = connect(&p).unwrap();
    koios::index_file(&conn, "report.md", RAW, "raw").unwrap();

    // Force a multi-line list into the index the way a derived page does.
    koios::index_file(&conn, "wiki/concepts/hub.md", WIKI_LIST, "wiki").unwrap();

    let raw: String = conn
        .query_row(
            "SELECT raw FROM blocks WHERE id='wiki/concepts/hub.md#Hub/1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        raw.lines().count(),
        3,
        "a three-line list must index as three lines (was joined with spaces):\n{raw:?}"
    );
    assert!(
        raw.contains('\n'),
        "interior newlines must survive indexing:\n{raw:?}"
    );

    // The downstream consequence: line-oriented lint sees three findings.
    let findings = koios::lint::check_unsourced_assertions(&conn);
    let n = findings
        .iter()
        .filter(|f| f.contains("wiki/concepts/hub.md#Hub/1"))
        .count();
    assert_eq!(
        n, 3,
        "each list line is its own assertion — got {n}:\n{findings:#?}"
    );

    let _ = fs::remove_dir_all(&p);
}

/// §9.3 hub pages: cited by many, citing nothing themselves. Checking only the
/// outbound direction flagged the pages most worth keeping as garbage.
#[test]
fn orphan_check_honours_inbound_citations() {
    let p = vault("hub");
    fs::write(p.join("raw").join("report.md"), RAW).unwrap();
    let conn = connect(&p).unwrap();
    koios::index_file(&conn, "report.md", RAW, "raw").unwrap();
    koios::index_file(&conn, "wiki/concepts/hub.md", WIKI_LIST, "wiki").unwrap();

    // Hub has no outbound `[[` at all — the naive check calls it orphaned.
    let orphan_without_links = koios::lint::check_orphan_pages(&conn);
    assert!(
        orphan_without_links
            .iter()
            .any(|f| f.contains("hub.md") && f.contains("no links in or out")),
        "expected hub.md orphaned while nothing cites it:\n{orphan_without_links:#?}"
    );

    // One inbound citation (`[[raw/report.md]]`, with the layer prefix a
    // written link carries) is enough to make it a legitimate entry point.
    conn.execute(
        "INSERT INTO links(src,dst,kind,weight) VALUES('wiki/concepts/a.md','[[raw/wiki/concepts/hub.md]]','wikilink',1.0)",
        [],
    )
    .unwrap();
    let orphans = koios::lint::check_orphan_pages(&conn);
    assert!(
        !orphans.iter().any(|f| f.contains("hub.md")),
        "a cited page is not an orphan, even with no outgoing links:\n{orphans:#?}"
    );

    let _ = fs::remove_dir_all(&p);
}
