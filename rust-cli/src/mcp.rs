//! MCP server (§11) — mirrors koiosbase/mcp/server.py.
//!
//! Implemented against the protocol directly (JSON-RPC 2.0 over stdio) rather
//! than through a framework, for the same reason as the Python side: the SDK's
//! entry points were renamed between major versions and a hard dependency
//! would break offline installs and CI.
//!
//! Memory boundary (§11): KoiosBase stores curated, auditable semantic
//! knowledge. Session memory and user preferences belong to the agent side and
//! must NOT be written back.

/// Server-wide guidance returned at `initialize`. See the note at the call site
/// for why it exists; keep it short enough to survive host truncation.
const SERVER_INSTRUCTIONS: &str = "\
KoiosBase is a Markdown knowledge base. Every tool needs an absolute `vault` \
path; there is no current-vault state. Prefer `koios_ask` for an answer whose \
facts carry [[raw/...]] citations, `koios_search` for raw evidence. Never \
present an uncited sentence as fact: if the answer lacks citations or says \
资料中未涉及, the vault does not have it — do not fill the gap yourself.";

use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;

use crate::pipeline::Ev;

pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// JSON-RPC allows these to arrive without an id; they are never answered.
/// Mirrors koiosbase/mcp/server.py NOTIFICATION_METHODS verbatim.
const NOTIFICATION_METHODS: [&str; 3] = [
    "notifications/initialized",
    "notifications/cancelled",
    "initialized",
];

/// Handle a request that carries no `id` — a JSON-RPC notification.
///
/// Notifications MUST NOT be answered. The previous loop fell through to the
/// generic `method not found` arm and replied with an error bearing id=null,
/// which spec-compliant clients treat as a spurious unsolicited response and
/// which deadlocks hosts waiting for the next real id.
///
/// Returns true if the request was a notification (i.e. must not be answered).
fn handle_notification(req: &Value) -> bool {
    let Some(method) = req.get("method").and_then(|m| m.as_str()) else {
        return false;
    };
    if NOTIFICATION_METHODS.contains(&method) {
        return true;
    }
    // Anything else without an id is malformed; log rather than reply.
    eprintln!("[mcp] ignoring unhandled notification: {method}");
    true
}

/// Is this request a notification (no id)? Python checks `"id" not in req or
/// req["id"] is None` — a JSON null id counts as absent.
fn is_notification(req: &Value) -> bool {
    match req.get("id") {
        None => true,
        Some(Value::Null) => true,
        Some(_) => false,
    }
}

/// Set false to silence write attribution — mirrors Python's WRITE_AUDIT_LOG.
const WRITE_AUDIT_LOG: bool = true;

fn tool_defs() -> Value {
    json!([
      {
        "name": "koios_search",
        "description": "Search a KoiosBase vault. Returns evidence blocks with citation ids (P6).",
        "inputSchema": {
          "type": "object",
          "properties": {
            "vault": {"type": "string", "description": "vault directory"},
            "question": {"type": "string"},
            "top": {"type": "integer", "default": 8},
            "groups": {"type": "array", "items": {"type": "string"},
                       "description": "principal groups for ACL (§9.2)"}
          },
          "required": ["vault", "question"]
        }
      },
      {
        "name": "koios_ask",
        "description": "Ask a question and get an answer honoring the citation and refusal contracts (§6.5).",
        "inputSchema": {
          "type": "object",
          "properties": {
            "vault": {"type": "string"},
            "question": {"type": "string"},
            "top": {"type": "integer", "default": 8},
            "groups": {"type": "array", "items": {"type": "string"}}
          },
          "required": ["vault", "question"]
        }
      },
      {
        "name": "koios_write_answer",
        "description": "Write an approved answer back as a draft page (§6.6).",
        "inputSchema": {
          "type": "object",
          "properties": {
            "vault": {"type": "string"},
            "question": {"type": "string"},
            "answer": {"type": "string"},
            "groups": {"type": "array", "items": {"type": "string"},
                       "description": "caller groups; evidence is gathered with \
    these so citations never point at unreadable blocks (§9.2)"}
          },
          "required": ["vault", "question", "answer"]
        }
      }
    ])
}

fn groups_of(args: &Value) -> Option<Vec<String>> {
    let g = args.get("groups")?.as_array()?;
    let v: Vec<String> = g
        .iter()
        .filter_map(|x| x.as_str().map(|s| s.to_string()))
        .collect();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

fn as_str(args: &Value, k: &str) -> Result<String, String> {
    // Python indexes the dict directly and the tool wrapper stringifies the
    // resulting KeyError, so the wire message is "KeyError: '<key>'".
    args.get(k)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("KeyError: '{k}'"))
}

/// Python returns `dict(row)` from `SELECT * FROM blocks`, i.e. every column.
/// Matching that exactly keeps MCP payloads interchangeable between shells.
/// One row of `SELECT * FROM blocks`, in column order. Aliased because the
/// eleven-field tuple is unreadable inline and trips clippy's complexity lint.
type BlockRow = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    String,
);

fn blocks_to_json(conn: &Connection, blocks: &[Ev]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|b| {
            let mut stmt = conn
                .prepare("SELECT id,doc_path,section_id,type,breadcrumb,raw,hash,page_range,meta,ordinal,layer FROM blocks WHERE id=?")
                .ok()?;
            let row: BlockRow = stmt
                .query_row(rusqlite::params![&b.id], |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                        r.get::<_, String>(5).unwrap_or_default(),
                        r.get::<_, String>(6).unwrap_or_default(),
                        r.get(7)?, r.get(8)?,
                        r.get::<_, i64>(9).unwrap_or(0),
                        r.get::<_, String>(10).unwrap_or_else(|_| "raw".to_string()),
                    ))
                })
                .ok()?;
            let mut m = serde_json::Map::new();
            m.insert("id".into(), json!(row.0));
            m.insert("doc_path".into(), json!(row.1));
            m.insert("section_id".into(), json!(row.2));
            m.insert("type".into(), json!(row.3));
            m.insert("breadcrumb".into(), json!(row.4));
            m.insert("raw".into(), json!(row.5));
            m.insert("hash".into(), json!(row.6));
            m.insert("page_range".into(), json!(row.7));
            m.insert("meta".into(), json!(row.8));
            m.insert("ordinal".into(), json!(row.9));
            m.insert("layer".into(), json!(row.10));
            Some(Value::Object(m))
        })
        .collect()
}

fn tool_search(args: &Value) -> Result<Value, String> {
    let vault = Path::new(&as_str(args, "vault")?).to_path_buf();
    let question = as_str(args, "question")?;
    let top = args.get("top").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
    let conn = crate::connect(&vault).map_err(|e| e.to_string())?;
    // Python scores via hybrid_search then re-reads each row and attaches
    // `d["score"] = score`, so `score` is the LAST key in the emitted dict.
    let rows =
        crate::retrieval::hybrid_search(&conn, &question, top, true).map_err(|e| e.to_string())?;
    let ids: Vec<String> = rows.iter().map(|(i, _)| i.clone()).collect();
    let mut blocks: Vec<Ev> = ids
        .iter()
        .filter_map(|i| crate::pipeline::get_block(&conn, i))
        .collect();
    blocks = crate::acl::filter_blocks(&conn, blocks, groups_of(args).as_deref());
    blocks = crate::state::filter_visible(&conn, blocks);
    let mut out = blocks_to_json(&conn, &blocks);
    // attach score in retrieval order — Python does this inside the row loop
    for (item, (bid, score)) in out.iter_mut().zip(rows.iter()) {
        if let Value::Object(m) = item {
            if m.get("id").and_then(|v| v.as_str()) == Some(bid.as_str()) {
                m.insert("score".to_string(), json!(score));
            }
        }
    }
    Ok(json!({"blocks": out}))
}

fn tool_ask(args: &Value) -> Result<Value, String> {
    let vault = Path::new(&as_str(args, "vault")?).to_path_buf();
    let question = as_str(args, "question")?;
    let top = args.get("top").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
    let conn = crate::connect(&vault).map_err(|e| e.to_string())?;
    // MCP is a long-lived process over one vault; the config file is how it
    // learns the model, same as every other command.
    let cfg = crate::llm::load(&vault).unwrap_or_default();
    let res = if crate::llm::has_chat(&cfg) {
        let m = cfg.model.clone().unwrap_or_default();
        let f =
            move |q: &str, ctx: &str| -> Result<String, String> { crate::llm::chat(&m, q, ctx) };
        let top = cfg.retrieval.as_ref().and_then(|r| r.top).unwrap_or(top);
        crate::pipeline::full_query_with(
            &conn,
            &question,
            top,
            groups_of(args).as_deref(),
            Some(&f),
        )
    } else {
        crate::pipeline::full_query(&conn, &question, top, groups_of(args).as_deref())
    };
    let mut evidence = res.evidence;
    evidence = crate::acl::filter_blocks(&conn, evidence, groups_of(args).as_deref());
    let (cited, total) = crate::pipeline::citation_coverage(&res.answer);
    let mut violations = serde_json::Map::new();
    if total > 0 && cited < total {
        violations.insert(
            "citation".to_string(),
            json!(format!("{cited}/{total} sentences cited")),
        );
    }
    if evidence.is_empty() && !crate::pipeline::is_refusal(&res.answer) {
        violations.insert(
            "refusal".to_string(),
            json!("answer asserted with no evidence and no refusal marker"),
        );
    }
    // Python's trace carries question/hits/channel/verdict (+escalated);
    // reproducing all four keeps downstream consumers and tests interchangeable.
    let mut trace = serde_json::Map::new();
    trace.insert("question".into(), json!(question));
    trace.insert("hits".into(), json!(evidence.len()));
    trace.insert("channel".into(), json!("hybrid+ppr"));
    trace.insert(
        "verdict".into(),
        json!(crate::pipeline::grade(&question, &evidence)),
    );
    Ok(json!({
        "answer": res.answer,
        "evidence": blocks_to_json(&conn, &evidence),
        "violations": Value::Object(violations),
        "trace": Value::Object(trace),
    }))
}

fn tool_write_answer(args: &Value) -> Result<Value, String> {
    let vault = Path::new(&as_str(args, "vault")?).to_path_buf();
    let question = as_str(args, "question")?;
    let answer = as_str(args, "answer")?;
    let groups = groups_of(args);
    // §11 memory boundary: write-back mutates shared, versioned Markdown, so
    // "who wrote this and as whom" has to be reconstructable after the fact.
    // This is attribution, not authorization — the host owns access control.
    // Reproduced verbatim from koiosbase/mcp/server.py log_write() so stderr
    // is identical on both shells.
    if WRITE_AUDIT_LOG {
        let principal = match &groups {
            Some(g) if !g.is_empty() => {
                let mut s = g.clone();
                s.sort();
                s.join(",")
            }
            _ => "anonymous".to_string(),
        };
        eprintln!(
            "[koios-write] vault={} principal={} q={:?}",
            vault.display(),
            principal,
            question.chars().take(80).collect::<String>()
        );
    }
    let conn = crate::connect(&vault).map_err(|e| e.to_string())?;
    let ev = crate::pipeline::retrieve_channel(&conn, &question, 5, groups_of(args).as_deref());
    let path = crate::compile::write_answer_page(
        &vault,
        &question,
        &answer,
        &ev,
        &crate::state::now_date(),
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({"written": path.display().to_string(), "confidence": "draft"}))
}

/// Serialize like Python's `json.dumps(obj, ensure_ascii=False)`: `", "` and
/// `": "` separators, lists/objects on one line. serde_json has no such preset
/// (to_string is compact, to_string_pretty is multi-line), so it is hand-rolled.
fn python_dumps(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            let esc = s
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            format!("\"{esc}\"")
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(python_dumps).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, val)| format!(
                    "{}: {}",
                    python_dumps(&Value::String(k.clone())),
                    python_dumps(val)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Dispatch one JSON-RPC request and build the response.
pub fn handle_request(req: &Value) -> Value {
    let rid = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": rid, "result": result});
    let err = |code: i64, msg: String| json!({"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": msg}});

    match method {
        "initialize" => ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "koiosbase", "version": env!("CARGO_PKG_VERSION")},
            // Hosts that support `instructions` (Codex CLI, IDE extension,
            // ChatGPT desktop) load this as server-wide guidance alongside the
            // tools. It is the only place a cross-tool constraint can be
            // stated, and the constraint that matters here is not obvious from
            // the tool list: every vault path must be passed explicitly, and
            // the citation ids in results are the contract, not decoration.
            //
            // Kept under 512 chars so it survives truncation on hosts that cut
            // the field short.
            "instructions": SERVER_INSTRUCTIONS,
        })),
        "tools/list" => ok(json!({"tools": tool_defs()})),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let payload = match name {
                "koios_search" => tool_search(&arguments),
                "koios_ask" => tool_ask(&arguments),
                "koios_write_answer" => tool_write_answer(&arguments),
                other => return err(-32601, format!("unknown tool: {other}")),
            };
            match payload {
                // Python embeds `json.dumps(payload, ensure_ascii=False)`:
                // separators are ", " and ": " (not compact). Reproduced so the
                // MCP wire text is byte-identical between the two shells.
                Ok(p) => ok(json!({
                    "content": [{"type": "text", "text": python_dumps(&p)}]
                })),
                Err(e) => err(-32000, e),
            }
        }
        "ping" => ok(json!({})),
        other => err(-32601, format!("method not found: {other}")),
    }
}

/// Run the stdio JSON-RPC loop. One JSON object per line.
pub fn serve() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                let r = json!({"jsonrpc":"2.0","id":Value::Null,
                               "error":{"code":-32700,"message":"parse error"}});
                writeln!(out, "{}", python_dumps(&r))?;
                out.flush()?;
                continue;
            }
        };
        let shutdown = req.get("method").and_then(|m| m.as_str()) == Some("shutdown");
        // Mirrors Python's serve(): a request with no id is a notification and
        // must never be answered — replying with id=null breaks the
        // `notifications/initialized` handshake on spec-compliant hosts.
        if is_notification(&req) {
            handle_notification(&req);
            if shutdown {
                break;
            }
            continue;
        }
        // The whole envelope is serialized with Python separators too, so the
        // wire bytes (not just the payload) match the Python server.
        let resp = python_dumps(&handle_request(&req));
        writeln!(out, "{resp}")?;
        out.flush()?;
        if shutdown {
            break;
        }
    }
    Ok(())
}

/// Used by tests and by `--once` style callers: handle one raw line.
///
/// Returns "" for a NOTIFICATION — the same contract `serve()` implements, so a
/// test driving this helper exercises the real dispatch decision rather than a
/// separate code path.
pub fn handle_line_stdio(line: &str) -> String {
    let Ok(req) = serde_json::from_str::<Value>(line) else {
        return python_dumps(&json!({"jsonrpc":"2.0","id":Value::Null,
                                    "error":{"code":-32700,"message":"parse error"}}));
    };
    if is_notification(&req) {
        handle_notification(&req);
        return String::new();
    }
    python_dumps(&handle_request(&req))
}

/// The three tool definitions. Exposed for parity tests that assert the schema
/// a client sees matches koiosbase/mcp/server.py TOOLS.
pub fn tool_defs_for_test() -> Value {
    tool_defs()
}

#[allow(dead_code)]
pub fn handle_line(line: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(req) => handle_request(&req).to_string(),
        Err(_) => json!({"jsonrpc":"2.0","id":Value::Null,
                         "error":{"code":-32700,"message":"parse error"}})
        .to_string(),
    }
}

// ---------------------------------------------------------------------------
// Host registration
//
// The server above speaks MCP; this is how an agent host finds it. Two hosts,
// two different file formats, both documented by their vendors:
//
//  - Claude Code: `claude mcp add <name> -- <command>`, or a `mcpServers`
//    block in `.mcp.json` / `~/.claude.json`.
//  - Codex: `codex mcp add <name> -- <command>`, or a
//    `[mcp_servers.<name>]` table in `~/.codex/config.toml`.
//
// We prefer the vendor CLI when it is on PATH (it validates and lands in the
// right scope) and fall back to writing the file ourselves.
// ---------------------------------------------------------------------------

/// Which agent host we are registering with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    ClaudeCode,
    Codex,
}

impl Host {
    pub fn label(self) -> &'static str {
        match self {
            Host::ClaudeCode => "claude-code",
            Host::Codex => "codex",
        }
    }

    /// The binary to shell out to. Distinct from `label`: the host is
    /// "claude-code" but its executable is `claude`. Conflating the two made
    /// the vendor-CLI path silently unreachable for Claude, so every install
    /// fell through to hand-writing ~/.claude.json.
    fn cli(self) -> &'static str {
        match self {
            Host::ClaudeCode => "claude",
            Host::Codex => "codex",
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().replace('_', "-").as_str() {
            "claude" | "claude-code" | "claudecode" => Some(Host::ClaudeCode),
            "codex" | "openai-codex" => Some(Host::Codex),
            _ => None,
        }
    }

    /// Detect by looking for the host's CLI on PATH, then its config directory.
    fn detect() -> Option<Self> {
        let on_path = |b: &str| {
            std::env::var_os("PATH")
                .map(|p| {
                    std::env::split_paths(&p).any(|d| {
                        d.join(b).exists()
                            || d.join(format!("{b}.exe")).exists()
                            || d.join(format!("{b}.cmd")).exists()
                    })
                })
                .unwrap_or(false)
        };
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let has_dir = |d: &str| home.as_ref().map(|h| h.join(d).exists()).unwrap_or(false);
        if on_path("claude") || has_dir(".claude") {
            return Some(Host::ClaudeCode);
        }
        if on_path("codex") || has_dir(".codex") {
            return Some(Host::Codex);
        }
        None
    }
}

/// The binary path we register. Resolved from argv[0] so a user who installed
/// `koios` anywhere gets that exact path recorded, not whatever `koios` happens
/// to resolve to in the host's environment (which may differ or not exist).
fn self_exe() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "koios".to_string())
}

fn home_dir() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "HOME is not set".into())
}

fn run(cmd: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    std::process::Command::new(cmd).args(args).output()
}

/// `koios mcp --install` / `--uninstall`.
pub fn install(
    target: Option<&str>,
    uninstall: bool,
    host_flag: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let host = match host_flag {
        Some(h) => Host::from_name(h)
            .ok_or_else(|| format!("unknown host {h:?}; expected claude-code or codex"))?,
        None => match target {
            Some(t) => Host::from_name(t)
                .ok_or_else(|| format!("unknown host {t:?}; expected claude-code or codex"))?,
            None => {
                let all = !uninstall;
                if all {
                    // Installing with no host named: do every host we can see.
                    let detected: Vec<Host> = [Host::ClaudeCode, Host::Codex]
                        .into_iter()
                        .filter(|h| {
                            std::env::var_os("PATH")
                                .map(|p| {
                                    std::env::split_paths(&p).any(|d| {
                                        d.join(h.cli()).exists()
                                            || d.join(format!("{}.exe", h.cli())).exists()
                                    })
                                })
                                .unwrap_or(false)
                        })
                        .collect();
                    if detected.is_empty() {
                        return Err("no agent host found on PATH; pass \
                                    `koios mcp --install claude-code` or `--install codex`"
                            .into());
                    }
                    for h in detected {
                        install_one(h, false)?;
                    }
                    return Ok(());
                }
                Host::detect().ok_or(
                    "no agent host detected; pass `koios mcp --host claude-code` \
                     or `--host codex`",
                )?
            }
        },
    };
    install_one(host, uninstall)
}

fn install_one(host: Host, uninstall: bool) -> Result<(), Box<dyn std::error::Error>> {
    if uninstall {
        return uninstall_one(host);
    }
    let exe = self_exe();
    let cli = host.cli();
    // Preferred path: let the vendor CLI do it. It validates, picks a sane
    // scope, and stays correct if the config format changes.
    let out = run(cli, &["mcp", "add", "koios", "--", &exe, "mcp"]);
    let via_cli = matches!(&out, Ok(o) if o.status.success());
    if via_cli {
        println!("registered with {} via `{} mcp add`", host.label(), cli);
        println!("  command: {} mcp", exe);
    } else {
        install_by_file(host, &exe)?;
    }
    // The skills are orthogonal to the MCP registration: they are what tells
    // the model *how* to use the tools (cite or refuse), and they cost nothing
    // until loaded. One standard (`SKILL.md` + frontmatter), two directories.
    install_skill(host)?;
    Ok(())
}

fn uninstall_one(host: Host) -> Result<(), Box<dyn std::error::Error>> {
    let cli = host.cli();
    if let Ok(o) = run(cli, &["mcp", "remove", "koios"]) {
        if o.status.success() {
            println!("removed from {} via `{} mcp remove`", host.label(), cli);
            return Ok(());
        }
    }
    // No CLI: drop the entry from the file we would have written.
    match host {
        Host::ClaudeCode => {
            let p = home_dir()?.join(".claude.json");
            strip_claude_json(&p)?;
        }
        Host::Codex => {
            let p = home_dir()?.join(".codex").join("config.toml");
            strip_codex_toml(&p)?;
        }
    }
    Ok(())
}

fn install_by_file(host: Host, exe: &str) -> Result<(), Box<dyn std::error::Error>> {
    match host {
        Host::ClaudeCode => {
            let p = home_dir()?.join(".claude.json");
            let mut root: serde_json::Value = if p.exists() {
                serde_json::from_str(&std::fs::read_to_string(&p)?)?
            } else {
                serde_json::json!({})
            };
            let servers = root
                .as_object_mut()
                .ok_or("~/.claude.json is not a JSON object")?
                .entry("mcpServers")
                .or_insert_with(|| serde_json::json!({}));
            servers
                .as_object_mut()
                .ok_or("mcpServers is not an object")?
                .insert(
                    "koios".to_string(),
                    serde_json::json!({
                        "command": exe,
                        "args": ["mcp"],
                        // KoiosBase is offline and reads no secrets of its own; a
                        // model configured in a vault reads its own env var.
                        "type": "stdio"
                    }),
                );
            std::fs::write(&p, serde_json::to_string_pretty(&root)?)?;
            println!("wrote {} -> mcpServers.koios", p.display());
        }
        Host::Codex => {
            let dir = home_dir()?.join(".codex");
            std::fs::create_dir_all(&dir)?;
            let p = dir.join("config.toml");
            let existing = if p.exists() {
                std::fs::read_to_string(&p)?
            } else {
                String::new()
            };
            if existing.contains("[mcp_servers.koios]") {
                println!("{} already has [mcp_servers.koios]", p.display());
                return Ok(());
            }
            let block = format!(
                "\n[mcp_servers.koios]\ncommand = {}\nargs = [\"mcp\"]\n",
                toml_string(exe)
            );
            std::fs::write(&p, format!("{}{}", existing.trim_end(), block))?;
            println!("wrote {} -> [mcp_servers.koios]", p.display());
        }
    }
    println!("  command: {} mcp", exe);
    println!("  restart the host to pick it up");
    Ok(())
}

/// TOML basic string escaping. An exe path with a backslash or quote would
/// otherwise produce a config file the host cannot parse — and a host that
/// silently ignores a broken config looks exactly like one that found no tools.
fn toml_string(s: &str) -> String {
    let esc = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    format!("\"{esc}\"")
}

fn strip_claude_json(p: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if !p.exists() {
        return Ok(());
    }
    let mut root: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p)?)?;
    if let Some(obj) = root.as_object_mut() {
        if let Some(s) = obj.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
            s.remove("koios");
        }
    }
    std::fs::write(p, serde_json::to_string_pretty(&root)?)?;
    println!("removed mcpServers.koios from {}", p.display());
    Ok(())
}

fn strip_codex_toml(p: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if !p.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(p)?;
    let mut out = String::new();
    let mut skipping = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            skipping = t == "[mcp_servers.koios]";
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    std::fs::write(p, out)?;
    println!("removed [mcp_servers.koios] from {}", p.display());
    Ok(())
}

/// Ship the skill that teaches the model how to use the tools.
///
/// Registering the server gives the model *access*; it does not tell it that a
/// KoiosBase answer without a citation must be treated as "not found" rather
/// than filled in from its own knowledge. That is the one thing this project
/// actually promises, so it ships as a skill.
///
/// Both hosts follow the same `SKILL.md` + frontmatter standard; only the
/// directory differs.
fn install_skill(host: Host) -> Result<(), Box<dyn std::error::Error>> {
    let home = home_dir()?;
    let dir = match host {
        Host::ClaudeCode => home.join(".claude").join("skills"),
        // Codex reads `$HOME/.agents/skills` (USER scope).
        Host::Codex => home.join(".agents").join("skills"),
    };
    let dest = dir.join("koiosbase").join("SKILL.md");
    std::fs::create_dir_all(dest.parent().unwrap())?;
    std::fs::write(&dest, SKILL_MD)?;
    println!("  skill: {}", dest.display());
    Ok(())
}

/// Inlined rather than read at runtime: the binary must be a single file that
/// works from anywhere, not one that depends on being run next to a checkout.
const SKILL_MD: &str = include_str!("../../skills/koiosbase/SKILL.md");
