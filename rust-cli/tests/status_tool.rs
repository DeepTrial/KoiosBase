//! `koios_status` reports the vector channel's state in a form a client can
//! branch on.
//!
//! Everything else in the MCP server returns free-text errors, so a client
//! could not tell "no model installed" from "this index came from a different
//! model" without matching sentences. This tool is the machine-readable half.

use std::fs;
use std::path::PathBuf;

use koios::connect;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-st-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    koios::init_vault(&p).unwrap();
    p
}

#[test]
fn status_says_model_missing_and_channel_inactive() {
    let v = vault("nomodel");
    let s = koios::embed::status(&v);
    assert!(!s.model_installed, "fixture has no model files");
    assert!(!s.vector_channel_active, "no model -> no semantic recall");
    assert!(s.error.is_some(), "status must explain why, not just bool");
}

#[test]
fn status_reflects_the_opt_out() {
    let v = vault("optout");
    koios::embed::set_allow_lexical(true);
    let s = koios::embed::status(&v);
    assert!(s.lexical_only, "the opt-out must be visible");
    assert!(
        !s.vector_channel_active,
        "opting out must not still claim semantic recall"
    );
    // Thread-local, so this cannot leak into the parallel tests.
    koios::embed::set_allow_lexical(false);
    assert!(!koios::embed::allow_lexical(), "default restored");
}

#[test]
fn status_detects_an_index_built_by_another_dimension() {
    let v = vault("mismatch");
    koios::embed::set_allow_lexical(true);
    let conn = koios::embed::ensure_vec_db(&v).unwrap();
    // Simulate an index left behind by a different model, without doing real
    // encoding -- this is exactly the state check_identity() guards against.
    conn.execute(
        "INSERT INTO vec_meta (key, value) VALUES ('dim', '999')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO vec_meta (key, value) VALUES ('model_id', 'other-model')",
        [],
    )
    .unwrap();
    let s = koios::embed::status(&v);
    assert!(s.index_mismatch, "a foreign index must be flagged");
    assert_eq!(s.index_dim.as_deref(), Some("999"));
    assert_eq!(s.index_model_id.as_deref(), Some("other-model"));
    koios::embed::set_allow_lexical(false);
}

#[test]
fn status_tool_answers_over_mcp() {
    let v = vault("tool");
    koios::embed::set_allow_lexical(true);
    let args = serde_json::json!({"vault": v.to_str().unwrap()});
    let out = koios::mcp::tool_status(&args).expect("status must succeed");
    assert!(out.get("model_installed").is_some());
    assert!(out.get("vector_channel_active").is_some());
    assert!(
        out.get("dim").is_some(),
        "callers need the expected dimension"
    );
    assert!(out.get("error").is_some(), "error key must always exist");
    koios::embed::set_allow_lexical(false);
}

#[test]
fn status_does_not_load_the_model() {
    // A status call is meant to be cheap and side-effect free; running it
    // against a vault with no model must not throw.
    let v = vault("cheap");
    koios::embed::set_allow_lexical(true);
    let _ = koios::embed::status(&v);
    let _ = connect(&v).unwrap();
    koios::embed::set_allow_lexical(false);
}

#[test]
fn status_reports_whether_this_binary_can_embed_at_all() {
    let v = vault("buildmode");
    let s = koios::embed::status(&v);
    // The gate is build-level: a binary without ONNX Runtime must say so
    // rather than quietly answering lexically against a corpus that has
    // vectors -- the same two-behaviours trap, reached through cargo instead
    // of the filesystem.
    assert_eq!(s.ort_available, koios::embed::ort_available());
    assert!(
        s.vector_channel_active == (s.ort_available && s.model_installed && !s.lexical_only),
        "the channel must not claim activity it cannot deliver"
    );
}
