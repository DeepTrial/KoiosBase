"""End-to-end tests for the v0.1 pipeline: parse -> index -> retrieve -> generate."""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

from koiosbase.generation.contracts import (
    check_contracts,
    citation_coverage,
    is_refusal,
)
from koiosbase.generation.judge import judge
from koiosbase.ingest.pipeline import build_index
from koiosbase.lint.gardener import run_l1
from koiosbase.parsers.markdown import all_blocks, parse_markdown
from koiosbase.query.pipeline import assemble, query, retrieve
from koiosbase.retrieval.router import fts_search, hybrid_search, personalized_pagerank

MD = """---
title: ACME 2024年报
version: 3
---

# 财务分析

## 负债分析
ACME 公司 2024 年营收 32 亿元，负债合计 18 亿元。

| 项目 | 金额 |
|---|---|
| 营收 | 32 亿 |
| 负债 | 18 亿 |

## 现金流
经营性现金流为正，达到 7.5 亿元。
"""


@pytest.fixture
def vault(tmp_path: Path) -> Path:
    (tmp_path / "raw").mkdir()
    (tmp_path / "raw" / "report.md").write_text(MD, encoding="utf-8")
    return tmp_path


def test_parse_builds_tree_and_blocks(vault):
    doc = parse_markdown(vault / "raw" / "report.md", vault / "raw")
    blocks = all_blocks(doc)
    assert blocks, "parser produced no blocks"
    kinds = {b.type for b in blocks}
    assert "table" in kinds, "table must stay atomic (P2)"
    table = next(b for b in blocks if b.type == "table")
    assert "营收" in table.raw and "负债" in table.raw, "table must not be split"


def test_frontmatter_and_breadcrumb(vault):
    doc = parse_markdown(vault / "raw" / "report.md", vault / "raw")
    assert doc.frontmatter["title"] == "ACME 2024年报"
    b = next(b for b in all_blocks(doc) if b.type == "paragraph")
    assert "ACME 2024年报" in b.breadcrumb, "breadcrumb anchors reference (P6)"


def test_index_rebuild_counts_blocks(vault):
    n = build_index(vault)
    assert n >= 3
    # idempotent rebuild
    assert build_index(vault) == n


def test_fts_finds_exact_term(vault):
    build_index(vault)
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    hits = fts_search(conn, "营收", limit=5)
    assert hits, "BM25 channel returned nothing"
    assert hybrid_search(conn, "现金流", limit=5), "hybrid recall failed"


def test_retrieve_and_assemble_respects_budget(vault):
    build_index(vault)
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    blocks = retrieve(conn, "营收", top=5)
    assert blocks
    ctx = assemble(blocks, budget_chars=200)
    assert len(ctx) <= 400, "budget discipline violated (§6.4)"


def test_query_with_no_evidence_refuses(vault):
    build_index(vault)
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    res = query(conn, "火星殖民地预算是多少")
    assert res.trace["hits"] == 0
    assert is_refusal(res.answer), "must refuse rather than improvise (拒答契约)"


def test_citation_contract_flags_uncited_answer():
    ok = "ACME 营收 32 亿元 (raw/report.md#财务分析/负债分析/1)。"
    bad = "ACME 营收 32 亿元。"
    cited, total = citation_coverage(ok)
    assert cited == total == 1
    cited_b, total_b = citation_coverage(bad)
    assert cited_b == 0 and total_b == 1
    v = check_contracts(bad, [])
    assert "citation" in v


def test_ppr_runs_on_subgraph():
    adj = {"a": [("b", 1.0)], "b": [("c", 1.0)], "c": []}
    pr = personalized_pagerank(["a"], adj)
    assert pr and pr["a"] > pr["c"], "seed should outrank distant node"


def test_cross_family_judge_numbers():
    assert judge("营收 32 亿元", "营收 32 亿元") == "entailed"
    assert judge("营收 40 亿元", "营收 32 亿元") == "contradicted"
    assert judge("营收显著增长", "无数字段落") == "unknown"


def test_l1_detects_unsourced_wiki_assertion(vault):
    (vault / "wiki").mkdir()
    (vault / "wiki" / "e.md").write_text(
        "# 实体\n- 这家公司 2024 年净利为 5 亿元并且没有任何引用\n", encoding="utf-8"
    )
    build_index(vault)
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    findings = run_l1(conn)
    assert findings["unsourced_assertions"], "L1 must flag unsourced claims (P6)"
