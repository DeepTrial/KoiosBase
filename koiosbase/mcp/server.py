"""MCP server (design doc §11).

Exposes KoiosBase as MCP tools so external agents can search, ask and write.
Implemented against the protocol directly (JSON-RPC 2.0 over stdio) rather than
through a framework: the `mcp` SDK renamed its entry points between major
versions, and a hard dependency would break offline installs and CI. This keeps
the wire format standard while adding no runtime requirement.

Memory boundary (§11): KoiosBase stores curated, auditable semantic knowledge.
Session memory and user preferences belong to the agent side and must NOT be
written back — see the tool notes below.
"""

from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path
from typing import Any

from .. import __version__
from ..index.schema import connect
from ..query.pipeline import query
from ..security.acl import filter_rows
from ..state.model import filter_visible
from ..studio.exports import _load_blocks

PROTOCOL_VERSION = "2024-11-05"
# JSON-RPC allows these to arrive without an id; they are never answered.
NOTIFICATION_METHODS = {
    "notifications/initialized",
    "notifications/cancelled",
    "initialized",
}

# Set false to silence write attribution — see log_write.
WRITE_AUDIT_LOG = True

TOOLS = [
    {
        "name": "koios_search",
        "description": "Search a KoiosBase vault. Returns evidence blocks with "
        "citation ids (P6).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vault": {"type": "string", "description": "vault directory"},
                "question": {"type": "string"},
                "top": {"type": "integer", "default": 8},
                "groups": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "principal groups for ACL (§9.2)",
                },
            },
            "required": ["vault", "question"],
        },
    },
    {
        "name": "koios_ask",
        "description": "Ask a question and get an answer honoring the citation "
        "and refusal contracts (§6.5).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "vault": {"type": "string"},
                "question": {"type": "string"},
                "top": {"type": "integer", "default": 8},
                "groups": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["vault", "question"],
        },
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
                "groups": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "caller groups; evidence is gathered with "
                    "these so citations never point at unreadable blocks (§9.2)",
                },
            },
            "required": ["vault", "question", "answer"],
        },
    },
]


def log_write(question: str, vault: str, groups: set[str] | None) -> None:
    """Record every write-back to stderr (§11 memory boundary).

    Not authorization — attribution. Answer write-back mutates shared, versioned
    Markdown, so "who wrote this and as whom" has to be reconstructable after the
    fact. Kept as a log line rather than a policy so the agent host owns access
    control.
    """
    if not WRITE_AUDIT_LOG:
        return
    principal = ",".join(sorted(groups)) if groups else "anonymous"
    print(
        f"[koios-write] vault={vault} principal={principal} q={question[:80]!r}",
        file=sys.stderr,
    )


def _groups(args: dict) -> set[str] | None:
    g = args.get("groups")
    return set(g) if g else None


def tool_search(args: dict) -> dict:
    """Search — evidence already ACL-filtered inside retrieve (§9.2).

    `tool_search` previously post-filtered here, which is the wrong order for a
    subtler reason than it looks: filtering AFTER ranking means a restricted
    block has already consumed a top-k slot, so the caller gets k-1 results
    instead of k legitimate ones. Passing groups down keeps rank and filter
    consistent.
    """
    from ..retrieval.router import hybrid_search

    vault = Path(args["vault"])
    conn = connect(vault / ".index")
    conn.row_factory = sqlite3.Row
    rows = hybrid_search(conn, args["question"], limit=args.get("top", 8))
    blocks = _load_blocks(conn, [bid for bid, _s in rows])
    blocks = filter_rows(conn, blocks, _groups(args))
    blocks = filter_visible(blocks, conn)
    conn.close()
    return {"blocks": blocks}


def tool_ask(args: dict) -> dict:
    """Ask — groups flow INTO retrieval, not applied to the result afterwards.

    The old code called query() anonymously (so an entitled caller silently got
    anonymous-recall results — fail-closed but wrong) and then filtered with the
    real groups, which could only remove evidence the anonymous pass had already
    failed to find.
    """
    from ..state.model import filter_visible

    vault = Path(args["vault"])
    conn = connect(vault / ".index")
    res = query(
        conn, args["question"], top=args.get("top", 8), principal_groups=_groups(args)
    )
    # Defense in depth: `query` already applies both filters; repeating them costs
    # one cheap query and guarantees a stray code path cannot widen what the
    # caller sees. Nothing here can ADD anything.
    res.evidence = filter_visible(res.evidence, conn)
    res.evidence = filter_rows(conn, res.evidence, _groups(args))
    conn.close()
    return {
        "answer": res.answer,
        "evidence": res.evidence,
        "violations": res.violations,
        "trace": res.trace,
    }


def tool_write_answer(args: dict) -> dict:
    """Write an approved answer back — restricted sources cannot be cited (§9.2).

    Evidence is gathered with the caller's groups so the generated citations only
    point at blocks the caller was entitled to see; otherwise `依据` would become
    a second, indirect leak channel.
    """
    from datetime import datetime, timezone

    from ..compile.gate import write_answer_page

    vault = Path(args["vault"])
    conn = connect(vault / ".index")
    res = query(conn, args["question"], top=5, principal_groups=_groups(args))
    log_write(args.get("question", ""), str(vault), _groups(args))
    path = write_answer_page(
        vault,
        args["question"],
        args["answer"],
        res.evidence,
        datetime.now(timezone.utc).date().isoformat(),
    )
    conn.close()
    return {"written": str(path), "confidence": "draft"}


HANDLERS = {
    "koios_search": tool_search,
    "koios_ask": tool_ask,
    "koios_write_answer": tool_write_answer,
}


def handle_notification(req: dict) -> None:
    """Handle a request that carries no `id` — a JSON-RPC notification.

    Notifications (`notifications/initialized`, etc.) MUST NOT be answered. The
    old loop replied with an error bearing id=None, which spec-compliant clients
    treat as a spurious unsolicited response and which deadlocks hosts waiting
    for the next real id.
    """
    method = req.get("method")
    if method in NOTIFICATION_METHODS:
        return
    # Anything else without an id is malformed; log rather than reply.
    print(f"[mcp] ignoring unhandled notification: {method}", file=sys.stderr)


def handle_request(req: dict) -> dict:
    """Dispatch one JSON-RPC request and build the response."""
    rid = req.get("id")
    method = req.get("method")
    params = req.get("params") or {}

    def ok(result: Any) -> dict:
        return {"jsonrpc": "2.0", "id": rid, "result": result}

    def err(code: int, msg: str) -> dict:
        return {"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": msg}}

    if method == "initialize":
        return ok(
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "koiosbase", "version": __version__},
            }
        )
    if method == "tools/list":
        return ok({"tools": TOOLS})
    if method == "tools/call":
        name = params.get("name")
        handler = HANDLERS.get(name)
        if handler is None:
            return err(-32601, f"unknown tool: {name}")
        try:
            payload = handler(params.get("arguments") or {})
        except Exception as exc:  # noqa: BLE001 - tool errors must not kill the loop
            return err(-32000, f"{type(exc).__name__}: {exc}")
        return ok(
            {
                "content": [
                    {"type": "text", "text": json.dumps(payload, ensure_ascii=False)}
                ]
            }
        )
    if method == "ping":
        return ok({})
    return err(-32601, f"method not found: {method}")


def serve(stdin=None, stdout=None) -> int:
    """Run the stdio JSON-RPC loop. One JSON object per line."""
    stdin = stdin or sys.stdin
    stdout = stdout or sys.stdout
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            stdout.write(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": None,
                        "error": {"code": -32700, "message": "parse error"},
                    }
                )
                + "\n"
            )
            stdout.flush()
            continue
        resp = None
        if "id" not in req or req.get("id") is None:
            # Notification (§ JSON-RPC 2.0): no response, ever. See
            # handle_notification — answering here is what broke
            # `notifications/initialized` handshakes.
            handle_notification(req)
        else:
            resp = handle_request(req)
            stdout.write(json.dumps(resp, ensure_ascii=False) + "\n")
            stdout.flush()
        if req.get("method") == "shutdown":
            break
    return 0
