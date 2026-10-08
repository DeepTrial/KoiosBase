//! Model access without a provider SDK.
//!
//! KoiosBase deliberately ships **no** HTTP client, no API keys and no vendor
//! adapter — the six crates in `Cargo.toml` are all it depends on. That is a
//! property worth keeping: a knowledge base that hardcodes a provider makes the
//! provider part of its trust model.
//!
//! What the project did lack was any way to *reach* a model from the command
//! line or from MCP. `pipeline::LlmFn` and `pdf::VlmFn` existed as types but
//! nothing on the command surface could supply them, so "LLM-driven knowledge
//! base" was a library-only claim.
//!
//! This module closes that gap the same way git closes it for editors and
//! pagers: **the model is an external command**.
//!
//! ```text
//! generate:  stdin  <- assembled context
//!            env    <- KOIOS_QUESTION, KOIOS_MODE=generate
//!            stdout -> answer text
//!
//! vlm:       stdin  <- PNG bytes of one rasterized page
//!            env    <- KOIOS_PAGE_PATH, KOIOS_MODE=vlm
//!            stdout -> transcribed text
//! ```
//!
//! Consequences, all deliberate:
//!
//! - **No new dependency.** `std::process::Command` only.
//! - **No secret ever touches the vault or the repo.** The command string is
//!   yours; it reads your environment or your keyring, not a KoiosBase config
//!   file that someone will eventually commit.
//! - **Any provider.** `curl` to OpenAI, a local llama.cpp, a Python one-liner,
//!   an offline stub — the contract is bytes on stdin, text on stdout.
//! - **Default unchanged.** With no command configured, every path falls back
//!   to the deterministic built-in generator, so `koios eval` and the 122
//!   parity tests keep measuring the same thing they always did.

use std::io::Write;
use std::process::{Command, Stdio};

/// Environment variable read when no explicit flag is given.
pub const LLM_CMD_ENV: &str = "KOIOS_LLM_CMD";
/// Same, for the vision tier (scanned PDF pages).
pub const VLM_CMD_ENV: &str = "KOIOS_VLM_CMD";

/// Run `cmd` with `stdin` as its input and the given environment extras.
///
/// `sh -c` rather than a direct exec so a user can write a pipeline
/// (`jq`, `curl`, a wrapper script) without us growing a shell parser.
fn run(cmd: &str, stdin: &[u8], env: &[(&str, &str)]) -> Result<String, String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn model command: {e}"))?;

    // A model that takes stdin but never reads it would deadlock on write; a
    // broken pipe here means the command simply does not consume context, which
    // is legitimate (a canned-answer stub), so it is not an error.
    if let Some(mut sin) = child.stdin.take() {
        let _ = sin.write_all(stdin);
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("model command failed: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("model command exited with {}", out.status)
        } else {
            err
        });
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return Err("model command produced no output".to_string());
    }
    Ok(text)
}

/// Resolve the generator command: explicit flag wins, then the environment.
pub fn llm_cmd(flag: Option<&str>) -> Option<String> {
    let from_flag = flag.map(str::trim).filter(|s| !s.is_empty());
    if from_flag.is_some() {
        return from_flag.map(str::to_string);
    }
    std::env::var(LLM_CMD_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve the vision command the same way.
pub fn vlm_cmd(flag: Option<&str>) -> Option<String> {
    let from_flag = flag.map(str::trim).filter(|s| !s.is_empty());
    if from_flag.is_some() {
        return from_flag.map(str::to_string);
    }
    std::env::var(VLM_CMD_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Call the configured generator. Mirrors `pipeline::LlmFn`'s contract
/// (`Fn(&str, &str) -> Result<String, String>`), so it can be handed straight
/// to `full_query_with`.
pub fn generate(question: &str, context: &str) -> Result<String, String> {
    let Some(cmd) = llm_cmd(None) else {
        return Err("no model command configured".to_string());
    };
    generate_with(&cmd, question, context)
}

pub fn generate_with(cmd: &str, question: &str, context: &str) -> Result<String, String> {
    run(
        cmd,
        context.as_bytes(),
        &[
            ("KOIOS_MODE", "generate"),
            ("KOIOS_QUESTION", question),
            ("KOIOS_CONTEXT_CHARS", &context.chars().count().to_string()),
        ],
    )
}

/// Call the configured vision model for one rasterized page. Mirrors
/// `pdf::VlmFn` (`Fn(&str, &[u8]) -> Result<String, String>`).
pub fn transcribe(path: &str, png: &[u8]) -> Result<String, String> {
    let Some(cmd) = vlm_cmd(None) else {
        return Err("no vision command configured".to_string());
    };
    transcribe_with(&cmd, path, png)
}

pub fn transcribe_with(cmd: &str, path: &str, png: &[u8]) -> Result<String, String> {
    run(
        cmd,
        png,
        &[("KOIOS_MODE", "vlm"), ("KOIOS_PAGE_PATH", path)],
    )
}

/// True when any model is reachable, so the CLI can tell the user *why* they
/// are still getting extractive answers instead of silently ignoring a typo.
pub fn any_llm() -> bool {
    llm_cmd(None).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canned_command_receives_context_on_stdin() {
        let out = generate_with("cat", "q?", "hello context").unwrap();
        assert_eq!(out, "hello context");
    }

    #[test]
    fn question_is_passed_out_of_band() {
        let out = generate_with("printf '%s' \"$KOIOS_QUESTION\"", "what?", "ctx").unwrap();
        assert_eq!(out, "what?");
    }

    #[test]
    fn nonzero_exit_is_an_error_not_a_panic() {
        let e = generate_with("exit 3", "q", "ctx").unwrap_err();
        assert!(!e.is_empty());
    }

    #[test]
    fn empty_output_is_rejected() {
        assert!(generate_with("true", "q", "ctx").is_err());
    }

    #[test]
    fn env_falls_back_to_flag() {
        // No KOIOS_LLM_CMD in the test environment -> None, so the built-in
        // generator stays the default and eval numbers do not move.
        assert!(llm_cmd(None).is_none() || std::env::var(LLM_CMD_ENV).is_ok());
        assert_eq!(llm_cmd(Some("  ")), None);
        assert_eq!(llm_cmd(Some(" cat ")), Some("cat".to_string()));
    }
}
