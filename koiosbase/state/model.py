"""Knowledge state machine + cascading correction (design doc §8.2, §8.3).

P5 — knowledge has state, and a correction propagates. The three-layer model
makes the blast radius of an error a *deterministic graph query* rather than a
guess: mark a block retracted → reverse-index finds every page citing it → those
pages are marked stale → recompiled and logged.

States (§8.2): active | superseded | disputed | retracted | draft, plus a page
level `stale` flag meaning "compiled output is behind its sources".
"""

from __future__ import annotations

import sqlite3
from datetime import datetime, timezone
from pathlib import Path

VALID_STATES = {"active", "superseded", "disputed", "retracted", "draft"}
# Which states are excluded from retrieval by default (§8.2 disposal column).
FILTERED = {"superseded", "retracted"}

SCHEMA_EXTRA = """
CREATE TABLE IF NOT EXISTS block_state (
  block_id TEXT PRIMARY KEY,
  state TEXT NOT NULL DEFAULT 'active',
  reason TEXT,
  updated TEXT
);
CREATE TABLE IF NOT EXISTS page_state (
  page_path TEXT PRIMARY KEY,
  stale INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL DEFAULT 'active',
  reason TEXT,
  updated TEXT
);
"""


def ensure_tables(conn: sqlite3.Connection) -> None:
    conn.executescript(SCHEMA_EXTRA)
    conn.commit()


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def set_block_state(
    conn: sqlite3.Connection, block_id: str, state: str, reason: str = ""
) -> bool:
    if state not in VALID_STATES:
        raise ValueError(f"invalid state: {state}")
    ensure_tables(conn)
    conn.execute(
        "INSERT OR REPLACE INTO block_state(block_id,state,reason,updated)"
        " VALUES(?,?,?,?)",
        (block_id, state, reason, _now()),
    )
    conn.commit()
    return True


def get_block_state(conn: sqlite3.Connection, block_id: str) -> str:
    ensure_tables(conn)
    row = conn.execute(
        "SELECT state FROM block_state WHERE block_id=?", (block_id,)
    ).fetchone()
    return row[0] if row else "active"


def referencing_pages(
    conn: sqlite3.Connection, block_id: str, vault: str | Path | None = None
) -> list[str]:
    """Reverse index: which pages cite this block (§8.3 — the blast radius).

    Looks at the vault's wiki/ Markdown directly when a vault path is given.
    That matters because compiled pages (entities/, sources/, synthesis/) are
    DERIVED and deliberately kept out of the block index (P1), so querying only
    indexed rows would find zero references and silently swallow the cascade.
    Falls back to the indexed wiki rows when no vault path is supplied.
    """
    marker = f"[[raw/{block_id}]]"
    pages: list[str] = []
    if vault is not None:
        root = Path(vault) / "wiki"
        if root.exists():
            for p in sorted(root.rglob("*.md")):
                try:
                    text = p.read_text(encoding="utf-8")
                except (OSError, UnicodeDecodeError):
                    continue
                if marker in text:
                    pages.append(str(p.relative_to(Path(vault))))
        return pages
    rows = conn.execute(
        "SELECT DISTINCT doc_path FROM blocks WHERE layer='wiki' AND raw LIKE ?",
        (f"%{marker}%",),
    ).fetchall()
    return [r[0] for r in rows]


def mark_stale(conn: sqlite3.Connection, page_path: str, reason: str = "") -> None:
    ensure_tables(conn)
    conn.execute(
        "INSERT OR REPLACE INTO page_state(page_path,stale,state,reason,updated)"
        " VALUES(?,1,'active',?,?)",
        (page_path, reason, _now()),
    )
    conn.commit()


def is_stale(conn: sqlite3.Connection, page_path: str) -> bool:
    ensure_tables(conn)
    row = conn.execute(
        "SELECT stale FROM page_state WHERE page_path=?", (page_path,)
    ).fetchone()
    return bool(row[0]) if row else False


def cascade_retraction(
    conn: sqlite3.Connection,
    block_id: str,
    reason: str = "",
    vault: str | Path | None = None,
) -> dict:
    """Retract a block and propagate: every citing page goes stale (§8.3)."""
    set_block_state(conn, block_id, "retracted", reason)
    pages = referencing_pages(conn, block_id, vault=vault)
    for p in pages:
        mark_stale(conn, p, f"cited retracted {block_id}: {reason}")
    return {"block": block_id, "state": "retracted", "affected_pages": pages}


def filter_visible(rows: list[dict], conn: sqlite3.Connection) -> list[dict]:
    """Drop rows whose source block is superseded/retracted (§8.2)."""
    if not rows:
        return rows
    ensure_tables(conn)
    out = []
    for r in rows:
        bid = r.get("id") or r.get("block_id")
        if bid and get_block_state(conn, bid) in FILTERED:
            continue
        out.append(r)
    return out


# --- §8.3 query-time disposition (lazy synthesis update) -------------------


def disposition_for(
    conn: sqlite3.Connection, page_path: str, high_risk: bool = False
) -> str:
    """How a query should treat a page in this state (§8.3 table)."""
    ensure_tables(conn)
    row = conn.execute(
        "SELECT stale,state FROM page_state WHERE page_path=?", (page_path,)
    ).fetchone()
    if not row:
        return "use"
    _stale, state = row[0], row[1]
    if state in {"retracted"}:
        return "hard_filter"
    if state in {"superseded"}:
        return "filtered_unless_explicit_history"
    if row[0]:
        # default: degrade + async repair; only high-risk / sole-source escalates
        return "refuse_or_recompile" if high_risk else "downrank_and_async_recompile"
    return "use"
