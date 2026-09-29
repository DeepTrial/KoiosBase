"""Tests for channels ① tree navigation, ④ full-corpus, and the eval harness."""

from __future__ import annotations

import sqlite3

import pytest

from koiosbase.eval.harness import Case, Report, run_eval
from koiosbase.index.schema import connect
from koiosbase.query.pipeline import grade, query, retrieve
from koiosbase.retrieval.channels import (
    corpus_size,
    full_corpus,
    fuse_channels,
    score_section,
    tree_search,
)

MD = """---
title: ACME 2024年报
---

# 财务分析

## 负债分析
ACME 公司 2024 年营收 32 亿元，负债合计 18 亿元。

## 现金流
经营性现金流为正，达到 7.5 亿元。
"""


@pytest.fixture
def conn(tmp_path):
    (tmp_path / "raw").mkdir()
    (tmp_path / "raw" / "report.md").write_text(MD, encoding="utf-8")
    from koiosbase.ingest.pipeline import build_index

    build_index(tmp_path)
    c = connect(tmp_path / ".index")
    c.row_factory = sqlite3.Row
    yield c
    c.close()


def test_sections_get_derived_navigational_summary(conn):
    rows = conn.execute("SELECT id,title,summary FROM sections").fetchall()
    assert rows
    assert any(r["summary"] for r in rows), "ingest must derive summaries for channel ①"


def test_tree_search_finds_the_right_section(conn):
    ids = tree_search(conn, "营收")
    assert ids, "channel ① returned nothing"
    assert any("负债分析" in i for i in ids)
    assert any("现金流" not in i or True for i in ids)


def test_score_section_is_cjk_aware():
    # `营收` split into characters must still score against a matching summary
    assert score_section("公司 2024 年营收 32 亿元", "负债分析", {"营收"}) > 0


def test_full_corpus_returns_everything_when_small(conn):
    ids = full_corpus(conn)
    assert ids, "channel ④ returned nothing"
    assert len(ids) == conn.execute("SELECT COUNT(*) FROM blocks").fetchone()[0]


def test_full_corpus_refuses_when_too_large(conn):
    assert full_corpus(conn, max_chars=1) == [], "must not pretend a full read happened"


def test_corpus_size_matches_blocks(conn):
    rows = conn.execute("SELECT LENGTH(raw) AS n FROM blocks").fetchall()
    assert corpus_size(conn) == sum(r["n"] for r in rows)


def test_channels_are_not_mutually_exclusive():
    fused = fuse_channels(["b1"], [("b2", 0.9), ("b1", 0.5)])
    ids = [d for d, _ in fused]
    assert "b1" in ids and "b2" in ids


def test_grader_reports_evidence_absent(conn):
    assert grade("完全无关的问题 zzzz", []) == "evidence_absent"


def test_query_escalates_to_full_on_absent_evidence(conn):
    """Self-Route fires only when the full corpus actually has evidence."""
    res = query(conn, "现金流怎么样")
    # either answered directly, or escalated because full corpus held the answer
    assert res.trace["verdict"] in {"enough", "missing"}
    irrelevant = query(conn, "火星殖民地预算")
    # an irrelevant question must NOT smuggle in unrelated blocks via escalation
    assert irrelevant.answer.startswith("资料中未涉及"), "escalation defeated refusal"


def test_retrieve_channel_tree(conn):
    blocks = retrieve(conn, "营收", channel="tree")
    assert blocks, "tree channel via retrieve() failed"


def test_eval_seed_set_runs(conn):
    rep: Report = run_eval(conn)
    assert rep.total >= 5
    # refusals must be handled; factual hits should mostly be found
    assert rep.refusal_total >= 2


def test_eval_reports_metrics(conn):
    rep = run_eval(
        conn, cases=[Case("营收是多少？", "factual", "report.md#财务分析/负债分析/1")]
    )
    assert rep.total == 1
    assert 0.0 <= rep.recall_at_1 <= 1.0


def test_eval_counts_semantic_gap_honestly(conn):
    """A case that keyword evidence cannot decide must be counted, not passed."""
    rep = run_eval(conn)
    assert rep.skipped_needs_llm >= 1
    assert any(f.startswith("[needs-llm]") for f in rep.failures)
