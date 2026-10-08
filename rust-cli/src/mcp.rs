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
    // MCP is a long-lived process, so it reads $KOIOS_LLM_CMD rather than a
    // flag: the client that launched it controls the environment. Unset means
    // the deterministic generator, exactly as the CLI behaves.
    let res = match crate::llm::llm_cmd(None) {
        Some(cmd) => {
            let f = move |q: &str, ctx: &str| -> Result<String, String> {
                crate::llm::generate_with(&cmd, q, ctx)
            };
            crate::pipeline::full_query_with(
                &conn,
                &question,
                top,
                groups_of(args).as_deref(),
                Some(&f),
            )
        }
        None => crate::pipeline::full_query(&conn, &question, top, groups_of(args).as_deref()),
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
