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

from ..index.schema import connect
from ..query.pipeline import query
from ..security.acl import filter_rows

PROTOCOL_VERSION = "2024-11-05"

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
            },
            "required": ["vault", "question", "answer"],
        },
    },
]


def _groups(args: dict) -> set[str] | None:
    g = args.get("groups")
    return set(g) if g else None


def tool_search(args: dict) -> dict:
    from ..retrieval.router import hybrid_search

    vault = Path(args["vault"])
    conn = connect(vault / ".index")
    conn.row_factory = sqlite3.Row
    rows = hybrid_search(conn, args["question"], limit=args.get("top", 8))
    out = []
    for bid, score in rows:
        row = conn.execute("SELECT * FROM blocks WHERE id=?", (bid,)).fetchone()
        if row:
            d = dict(row)
            d["score"] = score
            out.append(d)
    out = filter_rows(conn, out, _groups(args))
    conn.close()
    return {"blocks": out}


def tool_ask(args: dict) -> dict:
    vault = Path(args["vault"])
    conn = connect(vault / ".index")
    res = query(conn, args["question"], top=args.get("top", 8))
    # ACL filter BEFORE returning: never hand a caller evidence it may not see.
    res.evidence = filter_rows(conn, res.evidence, _groups(args))
    conn.close()
    return {
        "answer": res.answer,
        "evidence": res.evidence,
        "violations": res.violations,
        "trace": res.trace,
    }


def tool_write_answer(args: dict) -> dict:
    from datetime import datetime, timezone

    from ..compile.gate import write_answer_page

    vault = Path(args["vault"])
    conn = connect(vault / ".index")
    res = query(conn, args["question"], top=5)
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
                "serverInfo": {"name": "koiosbase", "version": "0.8.0"},
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
        resp = handle_request(req)
        stdout.write(json.dumps(resp, ensure_ascii=False) + "\n")
        stdout.flush()
        if req.get("method") == "shutdown":
            break
    return 0
