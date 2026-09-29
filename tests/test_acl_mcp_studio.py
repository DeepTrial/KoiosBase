"""v0.8: ACL/multi-tenancy, MCP server, Studio exports."""

from __future__ import annotations

import json
import sqlite3

import pytest

from koiosbase.index.schema import connect
from koiosbase.ingest.pipeline import build_index
from koiosbase.mcp.server import TOOLS, handle_request
from koiosbase.query.pipeline import retrieve
from koiosbase.security.acl import allowed, filter_rows, grants_for
from koiosbase.studio.exports import render_brief, render_mindmap, studio_brief

PUBLIC_MD = "---\ntitle: 公开\n---\n# 概览\n营收 32 亿元。\n"
SECRET_MD = "---\ntitle: 机密\nacl: [finance-team]\n---\n# 薪酬\n高管薪酬 500 万。\n"


@pytest.fixture
def vault(tmp_path):
    (tmp_path / "raw").mkdir()
    (tmp_path / "raw" / "open.md").write_text(PUBLIC_MD, encoding="utf-8")
    (tmp_path / "raw" / "secret.md").write_text(SECRET_MD, encoding="utf-8")
    build_index(tmp_path)
    return tmp_path


def test_acl_grants_are_read_from_frontmatter(vault):
    conn = connect(vault / ".index")
    conn.row_factory = sqlite3.Row
    assert grants_for(conn, "open.md") is None, "no acl == public"
    assert grants_for(conn, "secret.md") == ["finance-team"]


def test_allowed_matrix():
    assert allowed(None, None) is True  # public, anonymous
    assert allowed(None, {"hr"}) is True
    assert allowed(["finance-team"], None) is False  # restricted, anonymous
    assert allowed(["finance-team"], {"hr"}) is False
    assert allowed(["finance-team"], {"finance-team"}) is True


def test_retrieval_filters_restricted_docs(vault):
    """§9.2: filter at retrieval, so restricted text never reaches the model."""
    conn = connect(vault / ".index")
    conn.row_factory = sqlite3.Row
    anon = retrieve(conn, "薪酬", top=5, principal_groups=None)
    assert all(r["doc_path"] != "secret.md" for r in anon), "anonymous leaked secret"
    fin = retrieve(conn, "薪酬", top=5, principal_groups={"finance-team"})
    assert any(r["doc_path"] == "secret.md" for r in fin), "entitled caller saw nothing"
    hr = retrieve(conn, "薪酬", top=5, principal_groups={"hr"})
    assert all(r["doc_path"] != "secret.md" for r in hr)


def test_public_docs_still_visible_to_anonymous(vault):
    conn = connect(vault / ".index")
    conn.row_factory = sqlite3.Row
    rows = retrieve(conn, "营收", top=5, principal_groups=None)
    assert any(r["doc_path"] == "open.md" for r in rows)


def test_filter_rows_respects_groups(vault):
    conn = connect(vault / ".index")
    conn.row_factory = sqlite3.Row
    rows = [{"doc_path": "open.md"}, {"doc_path": "secret.md"}]
    assert len(filter_rows(conn, rows, {"finance-team"})) == 2
    assert len(filter_rows(conn, rows, None)) == 1


def test_mcp_initialize_and_tool_list():
    res = handle_request({"id": 1, "method": "initialize"})["result"]
    assert res["serverInfo"]["name"] == "koiosbase"
    tools = handle_request({"id": 2, "method": "tools/list"})["result"]["tools"]
    assert {t["name"] for t in tools} == {
        "koios_search",
        "koios_ask",
        "koios_write_answer",
    }
    assert any(t["name"] == "koios_search" for t in TOOLS)


def test_mcp_search_returns_cited_blocks(vault):
    req = {
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "koios_search",
            "arguments": {"vault": str(vault), "question": "营收"},
        },
    }
    payload = json.loads(handle_request(req)["result"]["content"][0]["text"])
    assert payload["blocks"], "search returned nothing"
    assert all("id" in b for b in payload["blocks"]), "blocks must be citable (P6)"


def test_mcp_unknown_tool_errors():
    req = {"id": 4, "method": "tools/call", "params": {"name": "nope", "arguments": {}}}
    assert "error" in handle_request(req)


def test_studio_brief_carries_citations(vault):
    conn = connect(vault / ".index")
    conn.row_factory = sqlite3.Row
    path = studio_brief(conn, vault, "营收")
    text = path.read_text(encoding="utf-8")
    assert path.exists()
    assert "[[raw/" in text, "an export must cite its sources (P6)"


def test_render_brief_without_evidence_says_so():
    out = render_brief("空", [], "2026-09-29")
    assert "无证据" in out, "empty export must not invent filler"


def test_render_mindmap_sanitizes_mermaid_labels():
    out = render_mindmap("t", "root (x)", [("分支 [1]", ["叶子 (a)"])], "2026-09-29")
    assert "mindmap" in out
    assert "[" not in out.split("```mermaid")[1].split("```")[0].replace(
        "[1]", ""
    ).replace("[a]", ""), "brackets break mermaid labels"
