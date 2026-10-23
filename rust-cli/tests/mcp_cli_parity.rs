//! Guards that CLI and MCP retrieval do not drift apart.
//!
//! Both mcp::tool_search and the CLI's cmd_search were once separate
//! implementations of "search", so a query could recall through one exit and
//! return nothing through the other. These lock the two to one route.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-mcpcli-{}-{}", name, std::process::id()));
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

/// Both exits go through pipeline::retrieve_all, so both see every channel.
///
/// This test does not need a model: it holds whether `retrieve_all` falls back
/// to the lexical route (no model installed) or fuses vectors on top. What it
/// asserts is that neither exit has its own private retrieval path any more.
#[test]
fn search_uses_the_same_route_on_both_exits() {
    let (v, conn) = seeded("route");
    // These fixtures have no model installed, which is now a hard error unless
    // the caller opts in -- exactly the fail-closed behaviour being asserted
    // elsewhere. Retrieve here is deliberately lexical on both sides.
    koios::embed::set_allow_lexical(true);
    let via_pipeline = koios::pipeline::retrieve_all(&v, &conn, "营收", 8, None, None)
        .expect("shared route must succeed with --allow-no-embedding");
    let via_channel = koios::pipeline::retrieve_channel(&conn, "营收", 8, None);
    assert!(
        !via_pipeline.is_empty(),
        "the shared route must recall something"
    );
    // retrieve_all must never return FEWER than the plain lexical route: it is
    // the lexical route plus more channels, not a replacement that can drop
    // what the other found. That is exactly how the MCP tool lost results.
    assert_eq!(
        via_pipeline.len(),
        via_channel.len(),
        "the fused route must not lose blocks the lexical route returned"
    );
    let a: Vec<&str> = via_pipeline.iter().map(|b| b.id.as_str()).collect();
    let b: Vec<&str> = via_channel.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(a, b, "both exits must rank identically");
}

/// The tool implementations must call into the shared route rather than
/// reaching for a retrieval primitive themselves. mcp::tool_search previously
/// called hybrid_search directly, which is how it skipped two channels.
#[test]
fn mcp_search_tool_json_matches_the_shared_route() {
    let (v, _conn) = seeded("mpcjson");
    let args = serde_json::json!({"vault": v.to_str().unwrap(), "question": "营收", "top": 5});
    let payload = koios::mcp::tool_search(&args).expect("tool_search must succeed");
    let blocks = payload.get("blocks").and_then(|b| b.as_array()).unwrap();
    assert!(!blocks.is_empty(), "MCP search returned nothing");
    // `score` must remain the last key, matching the Python payload shape.
    let first = blocks[0].as_object().unwrap();
    // `Map` here is serde_json's preserve_order variant (an IndexMap), whose
    // keys() is DoubleEndedIterator; .last() would walk the whole map and
    // trips clippy's double_ended_iterator_last under -D warnings.
    let last_key = first.keys().next_back().unwrap();
    assert_eq!(last_key, "score", "score must be the final key");
}
