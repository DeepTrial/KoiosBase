"""v0.4: state machine, cascading correction, lazy synthesis, garden checks."""

from __future__ import annotations

import sqlite3

import pytest

from koiosbase.compile.synthesis import (
    mark_syntheses_stale,
    needs_recompile,
    recompile_synthesis,
)
from koiosbase.ingest.pipeline import build_index
from koiosbase.lint.gardener import (
    check_expired_ttl,
    check_stale_pages,
    run_l1,
)
from koiosbase.state.model import (
    cascade_retraction,
    disposition_for,
    ensure_tables,
    filter_visible,
    get_block_state,
    is_stale,
    referencing_pages,
    set_block_state,
)

MD = """---
title: 年报
---

# 财务分析
ACME 公司 2024 年营收 32 亿元。
"""


@pytest.fixture
def vault(tmp_path):
    (tmp_path / "raw").mkdir()
    (tmp_path / "raw" / "r.md").write_text(MD, encoding="utf-8")
    build_index(tmp_path)
    return tmp_path


def test_block_state_defaults_to_active(vault):
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    ensure_tables(conn)
    assert get_block_state(conn, "r.md#财务分析/1") == "active"
    set_block_state(conn, "r.md#财务分析/1", "retracted", "typo")
    assert get_block_state(conn, "r.md#财务分析/1") == "retracted"


def test_invalid_state_rejected(vault):
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    with pytest.raises(ValueError):
        set_block_state(conn, "x", "not-a-state")


def test_cascade_marks_citing_pages_stale(vault):
    """§8.3: retracting a block propagates to every page citing it."""
    # a wiki page that cites the raw block
    edir = vault / "wiki" / "entities"
    edir.mkdir(parents=True, exist_ok=True)
    (edir / "acme.md").write_text(
        "# ACME\n- 营收 32 亿 [[raw/r.md#财务分析/1]]\n", encoding="utf-8"
    )
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    ensure_tables(conn)

    pages = referencing_pages(conn, "r.md#财务分析/1", vault=vault)
    assert any("acme" in p for p in pages), "reverse index found no citing page"

    res = cascade_retraction(conn, "r.md#财务分析/1", "数字错误", vault=vault)
    assert res["affected_pages"], "cascade found nothing to update"
    assert get_block_state(conn, "r.md#财务分析/1") == "retracted"
    assert is_stale(conn, "wiki/entities/acme.md")


def test_disposition_when_stale(vault):
    """§8.3: stale defaults to degrade+async; only high-risk escalates."""
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    ensure_tables(conn)
    conn.execute(
        "INSERT OR REPLACE INTO page_state(page_path,stale,state,reason,updated)"
        " VALUES('wiki/synthesis/x.md',1,'active','src changed','now')"
    )
    conn.commit()
    assert (
        disposition_for(conn, "wiki/synthesis/x.md") == "downrank_and_async_recompile"
    )
    assert (
        disposition_for(conn, "wiki/synthesis/x.md", high_risk=True)
        == "refuse_or_recompile"
    )


def test_filter_visible_drops_retracted(vault):
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    ensure_tables(conn)
    set_block_state(conn, "r.md#财务分析/1", "retracted")
    rows = [{"id": "r.md#财务分析/1"}, {"id": "r.md#财务分析/2"}]
    assert filter_visible(rows, conn) == [{"id": "r.md#财务分析/2"}]


def test_lazy_synthesis(vault):
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    ensure_tables(conn)
    sdir = vault / "wiki" / "synthesis"
    sdir.mkdir(parents=True, exist_ok=True)
    (sdir / "finance.md").write_text("# outdated\n", encoding="utf-8")

    assert needs_recompile(conn, vault, "finance") is False
    mark_syntheses_stale(conn, vault)
    assert needs_recompile(conn, vault, "finance") is True, "lazy update not flagged"

    recompile_synthesis(
        conn, vault, "finance", ["acme"], [("营收 32 亿", "r.md#财务分析/1")]
    )
    assert needs_recompile(conn, vault, "finance") is False, "stale flag not cleared"
    assert "[[raw/" in (sdir / "finance.md").read_text(encoding="utf-8")


def test_lint_reports_stale_pages(vault):
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    ensure_tables(conn)
    conn.execute(
        "INSERT OR REPLACE INTO page_state(page_path,stale,state,reason,updated)"
        " VALUES('wiki/entities/a.md',1,'active','cited retracted','now')"
    )
    conn.commit()
    assert check_stale_pages(conn), "gardener must surface stale pages (§9.3)"


def test_lint_ttl_expiry(vault):
    conn = sqlite3.connect(str(vault / ".index" / "tree.db"))
    conn.row_factory = sqlite3.Row
    conn.execute(
        "UPDATE documents SET frontmatter=? WHERE path='r.md'",
        ('{"ttl": "1d", "valid_from": "2020-01-01"}',),
    )
    conn.commit()
    assert check_expired_ttl(conn), "expired TTL not detected (§9.1)"


def test_run_l1_survives_legacy_schema(tmp_path):
    """Old vaults lack the layer column; lint must degrade, not crash."""
    conn = sqlite3.connect(str(tmp_path / "legacy.db"))
    conn.row_factory = sqlite3.Row
    conn.execute("CREATE TABLE blocks (id TEXT, raw TEXT, doc_path TEXT)")
    conn.execute("CREATE TABLE documents (path TEXT, frontmatter TEXT)")
    conn.execute("CREATE TABLE links (src TEXT, dst TEXT, kind TEXT, weight REAL)")
    out = run_l1(conn)
    assert isinstance(out, dict) and "broken_links" in out
