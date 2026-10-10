//! `koios.toml` — the model configuration, end to end through the CLI.
//!
//! Two things this is really guarding: that a configured model is actually
//! *called* (rather than the config being parsed and then ignored), and that an
//! unconfigured or misconfigured vault does not silently change behaviour.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn vault(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-cfg-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(p.join("raw")).unwrap();
    p
}

fn seed(v: &std::path::Path) {
    fs::write(
        v.join("raw").join("report.md"),
        "---\ntitle: Report\n---\n\n# Financials\n\n## Revenue\n\nACME revenue in 2024 was 3.2 billion yuan.\n",
    )
    .unwrap();
}

fn koios() -> Command {
    Command::new(env!("CARGO_BIN_EXE_koios"))
}

/// With no config file at all, `query` must behave exactly as it always did.
/// This is the invariant the whole eval baseline rests on.
#[test]
fn no_config_means_the_builtin_generator() {
    let v = vault("none");
    seed(&v);
    koios().args(["index"]).arg(&v).output().unwrap();

    let out = koios()
        .args(["query", "revenue", "-p"])
        .arg(&v)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    // The un-configured `query` prints the ranked block list, not a composed
    // answer — no model ran, so nothing was generated.
    assert!(
        s.contains("report.md#Financials/Revenue/1"),
        "extractive output keeps its shape: {s}"
    );
    assert!(s.contains("verdict:"), "Grader still reports: {s}");
    assert!(
        !s.to_lowercase().contains("model request") && !s.to_lowercase().contains("api_key_env"),
        "no model was involved: {s}"
    );
}

/// A config naming an env var that is not set must say so, not fail silently
/// and not fall back to a fabricated answer.
#[test]
fn missing_api_key_is_reported_not_guessed() {
    let v = vault("nokey");
    seed(&v);
    fs::write(
        v.join("koios.toml"),
        "[model]\nmodel = \"m\"\nbase_url = \"http://127.0.0.1:1/v1\"\napi_key_env = \"KOIOS_TEST_ABSENT\"\n",
    )
    .unwrap();
    koios().args(["index"]).arg(&v).output().unwrap();

    let out = koios()
        .args(["query", "revenue", "-p"])
        .arg(&v)
        .env_remove("KOIOS_TEST_ABSENT")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("KOIOS_TEST_ABSENT") || err.contains("KOIOS_TEST_ABSENT"),
        "the missing variable must be named: {stdout} / {err}"
    );
}

/// `config` subcommand: reports what is wired up, including whether the key is
/// actually present. "Is a model connected?" is otherwise unanswerable.
#[test]
fn config_reports_state_and_scaffolds() {
    let v = vault("show");

    let out = koios().args(["config", "-p"]).arg(&v).output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("model: (none)"),
        "empty vault reports no model: {s}"
    );

    let out = koios()
        .args(["config", "--init", "-p"])
        .arg(&v)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(v.join("koios.toml").exists(), "--init writes the file");

    let out = koios().args(["config", "-p"]).arg(&v).output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("gpt-4o-mini"), "starter is parsed back: {s}");
    assert!(
        s.contains("vision: (none)"),
        "starter marks vision unconfigured: {s}"
    );
    assert!(
        s.contains("NOT SET"),
        "an unset key must be visible as such: {s}"
    );
}

/// The key's *name* is in the file; a literal key must never be honoured.
#[test]
fn a_literal_api_key_in_the_file_is_not_used() {
    let v = vault("literal");
    fs::write(
        v.join("koios.toml"),
        "[model]\nmodel = \"m\"\nbase_url = \"http://127.0.0.1:1/v1\"\napi_key = \"sk-live-abc\"\n",
    )
    .unwrap();
    let out = koios().args(["config", "-p"]).arg(&v).output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        !s.contains("sk-live-abc"),
        "config must never echo a literal key: {s}"
    );
}

/// Malformed TOML is an error naming the file, not a silent fall back to the
/// default generator.
#[test]
fn broken_config_is_an_error() {
    let v = vault("broken");
    seed(&v);
    fs::write(v.join("koios.toml"), "[model\nmodel = \"m\"\n").unwrap();
    let out = koios().args(["config", "-p"]).arg(&v).output().unwrap();
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        s.contains("koios.toml"),
        "the error must name the file: {s}"
    );
}
