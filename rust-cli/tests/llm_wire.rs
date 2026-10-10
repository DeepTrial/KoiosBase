//! The LLM wire format: two protocols, not two URLs.
//!
//! OpenAI and Anthropic differ in three places each — endpoint path, auth
//! header, and where the answer lives in the response — so supporting the
//! second is a separate request builder, not a base_url swap. These tests pin
//! all three for each, plus the two ways of choosing between them.
//!
//! They also lock the bug that made `koios query --llm-cmd` unusable on a vault
//! with no koios.toml: the command escape hatch used to sit BEHIND the
//! `model.model` check, so the one entry point promising "try a model without
//! configuring anything" was the one that demanded configuration.

use koios::llm::{extract_content, provider_of, ModelConfig, Provider};
use serde_json::json;

fn cfg(toml: &str) -> ModelConfig {
    toml::from_str(toml).expect("fixture must parse")
}

// ------------------------------------------------------------------ choosing --

#[test]
fn kind_selects_the_wire_format() {
    assert_eq!(
        provider_of(&cfg(r#"kind = "openai""#)).unwrap(),
        Provider::Openai
    );
    assert_eq!(
        provider_of(&cfg(r#"kind = "anthropic""#)).unwrap(),
        Provider::Anthropic
    );
    // "claude" names the model family, not the protocol; accept it so pointing
    // at Anthropic does not require knowing which magic word the parser wants.
    assert_eq!(
        provider_of(&cfg(r#"kind = "claude""#)).unwrap(),
        Provider::Anthropic
    );
}

#[test]
fn anthropics_own_url_is_detected_without_kind() {
    // Someone pasting https://api.anthropic.com/v1 should not have to also
    // learn that `kind` exists.
    let c = cfg(r#"base_url = "https://api.anthropic.com/v1""#);
    assert_eq!(provider_of(&c).unwrap(), Provider::Anthropic);

    let c = cfg(r#"base_url = "http://localhost:11434/v1""#);
    assert_eq!(provider_of(&c).unwrap(), Provider::Openai);
}

#[test]
fn an_unknown_kind_is_rejected_naming_the_valid_ones() {
    // A typo'd kind previously fell through to OpenAI silently, which is the
    // worst outcome: it "works" against the wrong endpoint and reports auth
    // errors that look like a bad key.
    let err = provider_of(&cfg(r#"kind = "gemini""#)).unwrap_err();
    assert!(err.contains("openai"), "{err}");
    assert!(err.contains("anthropic"), "{err}");
}

// ------------------------------------------------------------- reading answers --

#[test]
fn content_is_read_from_each_providers_own_shape() {
    let openai = json!({"choices": [{"message": {"content": "营收 32 亿 [[raw/r.md#a/1]]。"}}]});
    assert_eq!(
        extract_content(&openai, Provider::Openai).unwrap(),
        "营收 32 亿 [[raw/r.md#a/1]]。"
    );

    // Anthropic returns content blocks; the text ones carry the answer.
    let anthropic = json!({"content": [
        {"type": "text", "text": "营收 32 亿 "},
        {"type": "text", "text": "[[raw/r.md#a/1]]。"}
    ]});
    assert_eq!(
        extract_content(&anthropic, Provider::Anthropic).unwrap(),
        "营收 32 亿 [[raw/r.md#a/1]]。"
    );
}

#[test]
fn the_two_readers_do_not_accept_each_others_shape() {
    // The whole reason this is a branch rather than a try-both helper: silently
    // accepting the wrong shape is how a half-working integration ships.
    let openai = json!({"choices": [{"message": {"content": "hi"}}]});
    assert!(extract_content(&openai, Provider::Anthropic).is_none());
    let anthropic = json!({"content": [{"type": "text", "text": "hi"}]});
    assert!(extract_content(&anthropic, Provider::Openai).is_none());
}

#[test]
fn empty_content_is_none_so_the_caller_reports_the_error() {
    let blank = json!({"choices": [{"message": {"content": "   "}}]});
    assert!(extract_content(&blank, Provider::Openai).is_none());

    // Thinking blocks are not answers — a response of only reasoning summary
    // must not be mistaken for a refusal to participate.
    let thinking = json!({"content": [{"type": "thinking", "thinking": "hmm"}]});
    assert!(extract_content(&thinking, Provider::Anthropic).is_none());
}

// ---------------------------------------------------------------- escape hatch --

#[test]
fn command_beats_the_http_path_before_the_model_check() {
    // No model, no base_url, no key: the escape hatch must still work. This is
    // the regression that mattered — see the module doc.
    let c = cfg(r#"command = "printf %s hi""#);
    assert_eq!(provider_of(&c).unwrap(), Provider::Openai);
    assert!(c.model.is_none());
    assert!(c.base_url.is_none());
    // `has_chat` is what routes `query` through the model at all; a command
    // with no model name must still count as "a model is configured".
    let cfg_file = toml::from_str::<koios::llm::Config>(
        r#"[model]
command = "printf %s hi""#,
    )
    .unwrap();
    assert!(koios::llm::has_chat(&cfg_file));
}
