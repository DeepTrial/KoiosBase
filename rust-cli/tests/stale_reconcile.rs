//! The §8.3 write side, end to end: edit a source, re-index, and the synthesis
//! page built from it must come back stale — then `koios compile` must clear it.
//!
//! This is the test that was missing. Every piece existed and had unit tests
//! (`mark_syntheses_stale`, `needs_recompile`, `recompile_synthesis` all pass),
//! but **nothing called `mark_syntheses_stale` from `src/` at all** — so the
//! `page_state` table stayed empty however many times the vault was edited, and
//! the read side (`apply_disposition`, the （待更新） marker) had nothing to act
//! on. Unit-green / system-broken, the exact seam the parity doc warns about.
//! This exercises the real `cmd_index` + `cmd_compile` pair against a real vault.

use std::fs;
use std::path::{Path, PathBuf};

use koios::compile::reconcile_stale_syntheses;
use koios::{cmd_index, connect};

/// Fixtures carry no embedding model; fail-closed indexing requires the opt-in.
fn index(v: &std::path::Path) {
    koios::embed::set_allow_lexical(true);
    cmd_index(v).unwrap();
}

fn raw_md(body: &str) -> String {
    format!("---\ntitle: ACME\n---\n\n# Fin\n\n## Rev\n\n{body}\n")
}

fn vault(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("koios_stale_{}_{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("raw")).unwrap();
    d
}

fn staleness(v: &Path, key: &str) -> Option<i64> {
    let Ok(conn) = connect(v) else { return None };
    conn.query_row(
        "SELECT stale FROM page_state WHERE page_path=?",
        [key],
        |r| r.get::<_, i64>(0),
    )
    .ok()
}

#[test]
fn editing_a_source_flags_the_synthesis_page_stale() {
    let v = vault("flag");
    fs::write(v.join("raw").join("r.md"), raw_md("revenue was 32")).unwrap();
    index(&v);

    // A synthesis page that cites the doc — what `studio brief` writes.
    let sdir = v.join("wiki").join("synthesis");
    fs::create_dir_all(&sdir).unwrap();
    fs::write(
        sdir.join("brief-revenue.md"),
        "---\ntype: brief\ngenerated: true\ncreated: 2026-10-10\nsources: [raw/r.md#Fin/Rev/1]\n---\n\n# revenue\n",
    )
    .unwrap();

    // Re-indexing an UNCHANGED file must invalidate nothing — otherwise every
    // `index` on a quiet vault would dirty every export.
    index(&v);
    assert_eq!(
        staleness(&v, "synthesis/brief-revenue.md"),
        None,
        "an unchanged re-index flagged a page stale"
    );

    fs::write(v.join("raw").join("r.md"), raw_md("revenue was 99")).unwrap();
    index(&v);
    assert_eq!(
        staleness(&v, "synthesis/brief-revenue.md"),
        Some(1),
        "editing a cited source did not flag the synthesis page"
    );
}

#[test]
fn compile_rebuilds_brief_content_clears_stale_and_is_idempotent() {
    let v = vault("clear");
    fs::write(v.join("raw").join("r.md"), raw_md("revenue was 32")).unwrap();
    index(&v);
    let sdir = v.join("wiki").join("synthesis");
    fs::create_dir_all(&sdir).unwrap();
    let page = sdir.join("brief-revenue.md");
    fs::write(
        &page,
        "---\ntype: brief\ngenerated: true\ncreated: 2026-10-10\nsources: [raw/r.md#Fin/Rev/1]\n---\n\n# revenue\n\n## 要点\n\n- revenue was 32 [[raw/r.md#Fin/Rev/1]]\n",
    )
    .unwrap();

    fs::write(v.join("raw").join("r.md"), raw_md("revenue was 99")).unwrap();
    index(&v);
    assert_eq!(staleness(&v, "synthesis/brief-revenue.md"), Some(1));

    assert_eq!(
        reconcile_stale_syntheses(&connect(&v).unwrap(), &v, "2026-10-25"),
        1
    );
    assert_eq!(
        staleness(&v, "synthesis/brief-revenue.md"),
        Some(0),
        "compile did not clear the stale flag"
    );

    // Clearing the marker is not enough: the derived prose must move to the
    // current raw evidence too, while the original creation date survives.
    let text = fs::read_to_string(&page).unwrap();
    assert!(text.contains("revenue was 99"), "stale body survived: {text}");
    assert!(!text.contains("revenue was 32"), "old fact survived: {text}");
    assert!(text.contains("created: 2026-10-10"));
    assert!(text.contains("last_compiled: 2026-10-25"));

    // The marker must land INSIDE the frontmatter. An earlier implementation
    // compared lines by string value, matched the opening `---` as if it were
    // the closing one, and wrote the keys above the fence — a corrupt file that
    // every subsequent parser would misread.
    assert!(
        text.starts_with("---\n"),
        "frontmatter fence moved: {:?}",
        &text[..20.min(text.len())]
    );
    assert!(text.contains("stale: false"), "stale marker not written");
    assert!(text.contains("\n---\n"), "closing fence missing: {text:?}");

    // Second compile must be a no-op on the page — both in the db and on disk.
    let after_once = fs::read_to_string(&page).unwrap();
    reconcile_stale_syntheses(&connect(&v).unwrap(), &v, &koios::compile::stamp_now());
    assert_eq!(
        fs::read_to_string(&page).unwrap(),
        after_once,
        "recompiling an already-fresh page rewrote it"
    );
    let _ = fs::remove_dir_all(&v);
}

#[test]
fn a_fresh_vault_index_flags_nothing() {
    // `before_fp` is empty on a cold vault, which compares unequal to everything
    // if naively diffed — every page would be flagged on first contact.
    let v = vault("cold");
    fs::write(v.join("raw").join("r.md"), raw_md("revenue was 32")).unwrap();
    index(&v);
    let sdir = v.join("wiki").join("synthesis");
    fs::create_dir_all(&sdir).unwrap();
    fs::write(
        sdir.join("brief-revenue.md"),
        "---\ntype: brief\nsources: [raw/r.md#Fin/Rev/1]\n---\n\n# r\n",
    )
    .unwrap();
    // Second index now HAS history, and its fingerprints match.
    index(&v);
    assert_eq!(
        staleness(&v, "synthesis/brief-revenue.md"),
        None,
        "first index against prior history false-flagged a page"
    );
    let _ = fs::remove_dir_all(&v);
}

#[test]
fn synthesis_rebuild_preserves_human_review_and_conflict_notes() {
    let v = vault("synthesis");
    fs::write(v.join("raw").join("r.md"), raw_md("revenue was 32")).unwrap();
    index(&v);
    let sdir = v.join("wiki").join("synthesis");
    fs::create_dir_all(&sdir).unwrap();
    let page = sdir.join("revenue.md");
    fs::write(
        &page,
        "---\ntype: synthesis\nconfidence: medium\ngenerated: true\nlast_compiled: 2026-10-10\nstale: true\nsources: [raw/r.md#Fin/Rev/1]\n---\n\n# Revenue review\n\n## 综合结论\n- revenue was 32 [[raw/r.md#Fin/Rev/1]]\n\n## 覆盖实体\n- [[entities/acme]]\n\n## 矛盾与未决\n- 人工保留：待财务复核\n",
    )
    .unwrap();
    fs::write(v.join("raw").join("r.md"), raw_md("revenue was 99")).unwrap();
    index(&v);

    let conn = connect(&v).unwrap();
    koios::state::set_page_state(&conn, "synthesis/revenue.md", 1, "disputed", "human review")
        .unwrap();
    assert_eq!(reconcile_stale_syntheses(&conn, &v, "2026-10-25"), 1);

    let text = fs::read_to_string(&page).unwrap();
    assert!(text.contains("revenue was 99 [[raw/r.md#Fin/Rev/1]]"));
    assert!(!text.contains("revenue was 32"));
    assert!(text.contains("confidence: medium"));
    assert!(text.contains("- [[entities/acme]]"));
    assert!(text.contains("- 人工保留：待财务复核"));
    let (stale, state): (i64, String) = conn
        .query_row(
            "SELECT stale,state FROM page_state WHERE page_path='synthesis/revenue.md'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stale, 0);
    assert_eq!(state, "disputed", "compile overrode human review");
    let _ = fs::remove_dir_all(&v);
}

#[test]
fn unreplayable_pages_stay_stale_and_byte_identical() {
    let v = vault("unreplayable");
    fs::write(v.join("raw").join("r.md"), raw_md("revenue was 32")).unwrap();
    index(&v);
    let sdir = v.join("wiki").join("synthesis");
    fs::create_dir_all(&sdir).unwrap();
    let fixtures = [
        (
            "missing.md",
            "---\ntype: brief\ngenerated: true\ncreated: 2026-10-10\nsources: [raw/r.md#Fin/Rev/999]\n---\n\n# missing\n\nold body\n",
        ),
        (
            "map.md",
            "---\ntype: mindmap\ngenerated: true\ncreated: 2026-10-10\n---\n\n# map\n\nold map\n",
        ),
        (
            "manual.md",
            "---\ntype: brief\ngenerated: false\nsources: [raw/r.md#Fin/Rev/1]\n---\n\n# manual\n\nhuman body\n",
        ),
        (
            "unknown.md",
            "---\ntype: future-export\ngenerated: true\nsources: [raw/r.md#Fin/Rev/1]\n---\n\n# future\n\nfuture body\n",
        ),
    ];
    let conn = connect(&v).unwrap();
    for (name, text) in fixtures {
        fs::write(sdir.join(name), text).unwrap();
        koios::state::mark_stale(&conn, &format!("synthesis/{name}"), "test").unwrap();
    }

    assert_eq!(reconcile_stale_syntheses(&conn, &v, "2026-10-25"), 0);
    for (name, original) in fixtures {
        let key = format!("synthesis/{name}");
        assert_eq!(staleness(&v, &key), Some(1), "{name} was falsely cleared");
        assert_eq!(fs::read_to_string(sdir.join(name)).unwrap(), original);
    }
    let _ = fs::remove_dir_all(&v);
}
