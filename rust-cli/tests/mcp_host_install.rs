//! `koios mcp --install` — registering with an agent host.
//!
//! The MCP server itself is covered elsewhere; this covers the part that is
//! easy to get wrong and invisible when it breaks: a config file written in the
//! wrong shape (or to the wrong path) does not error, the host just reports no
//! tools, and the user cannot tell whether KoiosBase or the host is at fault.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fake_home(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("koios-mcp-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn koios() -> Command {
    Command::new(env!("CARGO_BIN_EXE_koios"))
}

/// Codex reads a `[mcp_servers.koios]` table in `~/.codex/config.toml`.
#[test]
fn codex_registration_writes_the_toml_table() {
    let h = fake_home("codex-install");
    let out = koios()
        .args(["mcp", "--install", "codex"])
        .env("HOME", &h)
        .env("PATH", "/nonexistent") // no `codex` CLI -> file fallback
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let cfg = fs::read_to_string(h.join(".codex").join("config.toml")).unwrap();
    assert!(
        cfg.contains("[mcp_servers.koios]"),
        "Codex needs the table header: {cfg}"
    );
    assert!(
        cfg.contains("args = [\"mcp\"]"),
        "server is `koios mcp`: {cfg}"
    );
    // The command must be an absolute path to THIS binary, not a bare `koios`
    // the host may not resolve.
    let cmd = cfg.lines().find(|l| l.starts_with("command")).unwrap();
    assert!(
        cmd.contains(std::env!("CARGO_BIN_EXE_koios")),
        "must record the resolved binary path: {cmd}"
    );
}

/// Claude Code reads `mcpServers` in `~/.claude.json`.
#[test]
fn claude_registration_writes_the_mcp_servers_block() {
    let h = fake_home("claude-install");
    let out = koios()
        .args(["mcp", "--install", "claude-code"])
        .env("HOME", &h)
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let txt = fs::read_to_string(h.join(".claude.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
    let srv = &v["mcpServers"]["koios"];
    assert_eq!(srv["args"][0], "mcp", "server is `koios mcp`: {srv}");
    assert!(
        srv["command"].as_str().unwrap().contains("koios"),
        "command recorded: {srv}"
    );
}

/// Installing twice must not duplicate the entry — a duplicated TOML table is
/// a parse error, and a host that fails to parse its config says nothing.
#[test]
fn installing_twice_is_idempotent() {
    let h = fake_home("idem");
    for _ in 0..2 {
        let out = koios()
            .args(["mcp", "--install", "codex"])
            .env("HOME", &h)
            .env("PATH", "/nonexistent")
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    let cfg = fs::read_to_string(h.join(".codex").join("config.toml")).unwrap();
    assert_eq!(
        cfg.matches("[mcp_servers.koios]").count(),
        1,
        "exactly one entry: {cfg}"
    );
}

/// When the vendor CLI exists it is preferred: it validates and picks the
/// right scope, and it keeps working if the file format changes.
#[test]
fn the_vendor_cli_is_preferred_over_writing_files() {
    let h = fake_home("vendor-cli");
    let bin = h.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = h.join("calls.log");
    for name in ["claude", "codex"] {
        let p = bin.join(name);
        fs::write(
            &p,
            format!(
                "#!/bin/sh\necho \"{name} $@\" >> \"{}\"\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    let out = koios()
        .args(["mcp", "--install", "claude-code"])
        .env("HOME", &h)
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        !h.join(".claude.json").exists(),
        "no file should be written when the CLI succeeds"
    );
    let calls = fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("mcp add koios --"),
        "the CLI must be invoked, got: {calls}"
    );
    assert!(
        calls.contains("mcp"),
        "args must include the mcp subcommand: {calls}"
    );
}

/// Uninstall removes what install wrote, for both hosts.
#[test]
fn uninstall_removes_the_entry() {
    let h = fake_home("uninstall");
    let env = |c: &mut Command| {
        c.env("HOME", &h).env("PATH", "/nonexistent");
    };
    let mut c = koios();
    c.args(["mcp", "--install", "codex"]);
    env(&mut c);
    assert!(c.output().unwrap().status.success());
    let mut c = koios();
    c.args(["mcp", "--install", "claude-code"]);
    env(&mut c);
    assert!(c.output().unwrap().status.success());

    let mut c = koios();
    c.args(["mcp", "--uninstall", "--host", "codex"]);
    env(&mut c);
    assert!(c.output().unwrap().status.success());
    let mut c = koios();
    c.args(["mcp", "--uninstall", "--host", "claude-code"]);
    env(&mut c);
    assert!(c.output().unwrap().status.success());

    let cfg = fs::read_to_string(h.join(".codex").join("config.toml")).unwrap();
    assert!(!cfg.contains("[mcp_servers.koios]"), "entry gone: {cfg}");
    let txt = fs::read_to_string(h.join(".claude.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
    assert!(v["mcpServers"]["koios"].is_null(), "entry gone: {txt}");
}

/// Unknown host names are an error, not a silent no-op.
#[test]
fn an_unknown_host_is_rejected() {
    let h = fake_home("unknown");
    let out = koios()
        .args(["mcp", "--install", "some-other-agent"])
        .env("HOME", &h)
        .output()
        .unwrap();
    assert!(!out.status.success(), "must fail");
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        s.to_lowercase().contains("expected"),
        "the message should say what is expected: {s}"
    );
}

/// Registering the server gives the model access; the skill is what tells it
/// that a KoiosBase answer without a citation must be treated as "not found"
/// rather than filled in from its own knowledge. Without it the tools work but
/// the guarantee does not.
#[test]
fn install_also_ships_the_skill_to_the_hosts_directory() {
    for (host, dir) in [
        ("claude-code", ".claude/skills"),
        ("codex", ".agents/skills"),
    ] {
        let h = fake_home(&format!("skill-{host}"));
        let out = koios()
            .args(["mcp", "--install", host])
            .env("HOME", &h)
            .env("PATH", "/nonexistent")
            .output()
            .unwrap();
        assert!(out.status.success(), "{host}: {:?}", out);

        let p = h.join(dir).join("koiosbase").join("SKILL.md");
        assert!(p.exists(), "{host}: skill missing at {}", p.display());
        let txt = fs::read_to_string(&p).unwrap();
        assert!(txt.starts_with("---\nname: koiosbase"), "{host}: {txt}");
        assert!(
            txt.contains("Never state a fact from the vault unless the answer carried a citation")
                || txt.contains("Never state a fact from the vault"),
            "{host}: the citation rule is the point of the skill"
        );
    }
}
