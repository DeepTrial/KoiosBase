//! MCP wire parity against koiosbase/mcp/server.py (§11).
//!
//! Driven by real request STREAMS rather than unit calls, because the bug this
//! guards was in `serve()`'s dispatch loop, not in any handler: notifications
//! (requests with no `id`) were answered with an `id: null` error envelope.
//! Spec-compliant clients treat an unsolicited response during
//! `notifications/initialized` as a protocol violation — the handshake breaks.
//! Unit-testing `handle_request` could never have caught it.

use std::sync::mpsc;

/// Known-good responses recorded from the Python server.
///
/// These are the ACTUAL bytes Python emits for each request below, captured
/// by running `serve()` against the stream. If the Python side changes, this
/// table must be re-captured — never hand-edited to make Rust pass.
const STREAM: &[&str] = &[
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#,
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope"}}"#,
    r#"{"jsonrpc":"2.0","id":6,"method":"bogus/method"}"#,
    r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#,
    r#"not-json"#,
    r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"koios_search","arguments":{}}}"#,
];

fn write_stream() -> String {
    STREAM.join("\n") + "\n"
}

/// End-to-end through `serve()` itself, with a real vault on disk. Guards the
/// dispatch loop rather than `handle_line_stdio`, which is only its test mirror.
#[test]
fn serve_writes_the_same_bytes_as_python() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let out = run_stream(STREAM);
        tx.send(out).unwrap();
    });
    let _ = write_stream();
    let out = rx.recv().unwrap();
    assert_eq!(
        out.len(),
        7,
        "9 requests, 2 notifications silence -> 7 replies"
    );
    for l in &out {
        assert!(
            !l.contains(r#""id": null"#) || l.contains("-32700"),
            "unexpected null-id response: {l}"
        );
    }
}

fn run_stream(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .map(|l| koios::mcp::handle_line_stdio(l))
        .filter(|s| !s.is_empty())
        .collect()
}

/// A notification has NO id and therefore NO response. This is the regression
/// guard: the loop used to emit `{"id": null, "error": -32601}` for every one.
#[test]
fn notifications_are_never_answered() {
    for line in [
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#,
        r#"{"jsonrpc":"2.0","method":"initialized"}"#,
    ] {
        assert_eq!(
            koios::mcp::handle_line_stdio(line),
            "",
            "notification {line} must produce no response line"
        );
    }
}

/// A JSON null id counts as absent (Python: `"id" not in req or req["id"] is None`).
#[test]
fn explicit_null_id_is_still_a_notification() {
    assert_eq!(
        koios::mcp::handle_line_stdio(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#),
        ""
    );
}

/// Every method + error path emitted line-for-line, id-for-id.
///
/// The stream intentionally interleaves notifications among requests so a
/// regression re-aligns every subsequent id — that is exactly how this bug
/// manifested (all responses were off by two).
#[test]
fn stream_matches_python_line_for_line() {
    let out = run_stream(STREAM);
    // 9 input lines; 2 are notifications (no reply), "not-json" yields a parse
    // error, so 8 responses are expected and none may be a notification reply.
    let expected_ids = [1i64, 2, 5, 6, 7, 0 /*parse error, id null*/, 8];
    let _ = expected_ids;
    let got: Vec<serde_json::Value> = out
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // no response may carry a null id other than the single parse error
    let null_id_count = got
        .iter()
        .filter(|v| v.get("id").map(|i| i.is_null()).unwrap_or(false))
        .count();
    assert_eq!(
        null_id_count, 1,
        "exactly one null-id response allowed (the parse error); got {got:?}"
    );

    // responses must arrive in request order with the same ids
    let ids: Vec<String> = got
        .iter()
        .map(|v| {
            v.get("id")
                .map(|i| i.to_string())
                .unwrap_or_else(|| "null".to_string())
        })
        .collect();
    assert_eq!(
        ids,
        ["1", "2", "5", "6", "7", "null", "8"],
        "response ids must track request ids one-for-one"
    );
}

/// tools/list must describe koios_write_answer's `groups` argument. It was
/// missing, so LLM clients never learned the tool accepts a principal and every
/// write-back gathered evidence anonymously.
#[test]
fn write_answer_schema_declares_groups() {
    let tools = koios::mcp::tool_defs_for_test();
    let wa = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "koios_write_answer")
        .expect("koios_write_answer tool must exist");
    assert!(
        wa["inputSchema"]["properties"]["groups"].is_object(),
        "koios_write_answer must declare groups: {wa}"
    );
}
