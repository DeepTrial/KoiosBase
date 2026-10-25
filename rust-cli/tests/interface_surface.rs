//! Caller-facing surface that used to be unanswerable: why a block ranked,
//! which corpus layer it belongs to, and whether the window was cut.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-if-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

fn seeded(name: &str) -> (PathBuf, rusqlite::Connection) {
    let v = vault(name);
    fs::write(
        v.join("raw").join("a.md"),
        "# 财务\n\n## 营收\n本公司 2025 年度营业收入为 120 亿元。\n",
    )
    .unwrap();
    koios::embed::set_allow_lexical(true);
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();
    (v, conn)
}

#[test]
fn every_block_reports_which_channels_matched_it() {
    let (v, conn) = seeded("channels");
    let out = koios::pipeline::retrieve_all(&v, &conn, "营收", 8, None, &Default::default())
        .expect("lexical route must succeed");
    assert!(!out.is_empty());
    for b in &out {
        let cs = b
            .channels
            .as_ref()
            .expect("the fused route must label every block");
        assert!(
            !cs.matched_by.is_empty(),
            "a returned block must say why it came back: {}",
            b.id
        );
    }
}

#[test]
fn layer_raw_omits_compiler_derived_pages() {
    let v = vault("layer");
    fs::write(v.join("raw").join("a.md"), "# A\n\nbody text here.\n").unwrap();
    koios::embed::set_allow_lexical(true);
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();

    // `index` itself generates wiki/sources/ (§4.3), so a derived page exists
    // here without a separate compile step.

    let derived_present =
        koios::pipeline::retrieve_all(&v, &conn, "body", 20, None, &Default::default())
            .unwrap()
            .iter()
            .any(|b| b.doc_path.starts_with("sources/"));
    assert!(derived_present, "fixture must contain a derived page");

    let raw_only = koios::pipeline::retrieve_all(
        &v,
        &conn,
        "body",
        20,
        None,
        &koios::pipeline::Query {
            layer: Some("raw"),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!raw_only.is_empty(), "raw-only must still find the source");
    assert!(
        !raw_only.iter().any(|b| b.doc_path.starts_with("sources/")),
        "layer=raw must not return derived pages"
    );
}

#[test]
fn source_blocks_outrank_their_own_derived_restatements() {
    let v = vault("crowding");
    fs::write(v.join("raw").join("a.md"), "# A\n\nunique sentence here.\n").unwrap();
    koios::embed::set_allow_lexical(true);
    koios::cmd_index(&v).unwrap();
    let conn = connect(&v).unwrap();

    let out =
        koios::pipeline::retrieve_all(&v, &conn, "unique sentence", 20, None, &Default::default())
            .unwrap();
    let first_source = out.iter().position(|b| !b.doc_path.starts_with("sources/"));
    let first_derived = out.iter().position(|b| b.doc_path.starts_with("sources/"));
    match (first_source, first_derived) {
        (Some(s), Some(d)) => assert!(
            s < d,
            "a document must rank above the page that merely restates it \
             (source at {s}, derived at {d})"
        ),
        _ => panic!("fixture needs both a source and a derived block"),
    }
}

#[test]
fn mcp_reports_truncation_and_scores() {
    let v = vault("mcp");
    fs::write(v.join("raw").join("a.md"), "# A\n\nbody text here.\n").unwrap();
    koios::embed::set_allow_lexical(true);
    koios::cmd_index(&v).unwrap();

    let args = serde_json::json!({"vault": v.to_str().unwrap(), "question": "body", "top": 1});
    let out = koios::mcp::tool_search(&args).expect("tool_search must succeed");
    assert!(
        out.get("truncated").is_some(),
        "clients need to know if the window was cut"
    );
    assert!(out.get("returned").is_some());
    let blocks = out.get("blocks").and_then(|b| b.as_array()).unwrap();
    assert!(!blocks.is_empty());
    // `score` must stay the LAST key: Python emits it last too, and the two
    // shells' payloads are diffed against each other.
    let last = blocks[0].as_object().unwrap().keys().next_back().unwrap();
    assert_eq!(last, "score");
}
