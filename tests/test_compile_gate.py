"""v0.3: compile layer, quality gate, answer write-back, PPR confidence weighting."""

from __future__ import annotations

import sqlite3

import pytest

from koiosbase.compile.entities import extract_entities, render_entity_page, slugify
from koiosbase.compile.gate import (
    CONFIDENCE_WEIGHT,
    can_promote,
    edge_weight,
    promote,
    read_confidence,
    write_answer_page,
)
from koiosbase.compile.pipeline import compile_vault
from koiosbase.ingest.pipeline import build_index
from koiosbase.parsers.markdown import all_blocks, parse_markdown
from koiosbase.retrieval.router import load_graph

MD = """---
title: 年报
---

# 财务分析
ACME 公司 2024 年营收 32 亿元。

## 负债分析
负债合计 18 亿元，涉及 [[raw/r.md#财务分析/1]]。
"""


@pytest.fixture
def vault(tmp_path):
    (tmp_path / "raw").mkdir()
    (tmp_path / "raw" / "r.md").write_text(MD, encoding="utf-8")
    build_index(tmp_path)
    return tmp_path


def test_entity_extraction_finds_orgs(vault):
    doc = parse_markdown(vault / "raw" / "r.md", vault / "raw")
    ents = extract_entities(all_blocks(doc))
    assert ents, "no entities extracted"
    assert any("acme" in eid for eid in ents)


def test_entity_page_carries_raw_citations(vault):
    doc = parse_markdown(vault / "raw" / "r.md", vault / "raw")
    ents = extract_entities(all_blocks(doc))
    ent = next(iter(ents.values()))
    page = render_entity_page(ent, "2026-09-29")
    assert "confidence: draft" in page, "new pages must start draft (§5.4)"
    assert "[[raw/" in page, "facts must cite raw (P6)"


def test_compile_writes_entity_pages(vault):
    summary = compile_vault(vault)
    assert summary["entity_pages"] >= 1
    assert (vault / "wiki" / "entities").exists()
    # affected set is computed, never a full scan
    assert "affected_pages" in summary


def test_promotion_requires_verification(vault):
    """Citation counts must never promote a page (§5.4)."""
    page = vault / "wiki" / "p.md"
    page.write_text(
        "---\ntype: entity\nconfidence: draft\n---\n# X\n", encoding="utf-8"
    )
    assert can_promote("draft") is False
    assert can_promote("draft", verified=True) is True
    assert can_promote("draft", human_confirmed=True) is True
    assert promote(page) == "draft", "promoted without verification"
    assert promote(page, verified=True) == "medium"
    assert read_confidence(page) == "medium"


def test_promotion_is_stepwise(tmp_path):
    page = tmp_path / "p.md"
    page.write_text("---\nconfidence: draft\n---\n", encoding="utf-8")
    assert promote(page, human_confirmed=True) == "medium"
    assert promote(page, human_confirmed=True) == "high"
    assert promote(page, human_confirmed=True) == "high"


def test_confidence_edge_weight_cuts_self_reinforcement():
    """draft pages confer no authority (§6.6) — the loop-breaking property."""
    assert CONFIDENCE_WEIGHT["draft"] == 0.0
    assert edge_weight("draft") == 0.0
    assert edge_weight("high") == 1.0
    assert edge_weight("medium") == 0.5


def test_ppr_drops_draft_edges(vault):
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    conn.execute(
        "INSERT INTO links(src,dst,kind,weight) VALUES('a','b','wikilink',1.0)"
    )
    conn.commit()
    full = load_graph(conn)
    weighted = load_graph(conn, page_confidence={"a": "draft"})
    assert full["a"], "baseline graph should contain the edge"
    assert not weighted.get("a"), "draft source must not confer authority"


def test_answer_write_back_is_draft_with_evidence(vault):
    evidence = [{"id": "r.md#财务分析/1"}]
    path = write_answer_page(vault, "营收是多少", "32 亿元", evidence, "2026-09-29")
    assert path.exists()
    text = path.read_text(encoding="utf-8")
    assert "confidence: draft" in text, "answers always start draft (§5.4)"
    assert "[[raw/r.md#财务分析/1]]" in text


def test_slugify_is_stable():
    assert slugify("ACME Corp") == "acme-corp"
    assert slugify("") == "entity"
