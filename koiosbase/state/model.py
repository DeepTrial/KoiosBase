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

    Looks at the vault's wiki/ Markdown directly when a vault path is given,
    because compiled pages are authored as files and their wikilinks need not
    correspond to any indexed row of their own. Falls back to the indexed wiki
    rows when no vault path is supplied.

    NOTE: this is no longer because "compiled pages are kept out of the block
    index". Derived pages (sources/, entities/, synthesis/, answers/) ARE
    indexed since v0.8.2 — see `ingest.pipeline._is_derived_page`. Reading the
    filesystem is still the reliable path, since a page may cite a heading that
    no block row resolves to.

    Heading-aware cite forms (all three resolve to the same source family):
      [[raw/{block_id}]]      block-level cite, as the compiler emits them
      [[raw/{doc_path}#...]]  section/heading-level cite (hand-written wiki)
      [[raw/{doc_path}]]      whole-document cite
    Matching only the first form silently dropped hand-written pages that cite a
    heading, so a retraction cascaded to fewer pages than actually quote it.

    Heading cites are matched by prefix: the index knows block ids, not rendered
    headings, so `[[raw/r.md#财务分析]]` must reach every block under that doc.
    """
    block_id = block_id.strip()
    doc_path = block_id.split("#", 1)[0]
    prefixes = tuple(
        p
        for p in (f"[[raw/{block_id}", f"[[raw/{doc_path}#", f"[[raw/{doc_path}]]")
        if p
    )
    if not prefixes:
        return []
    if block_id == doc_path:
        prefixes = tuple(dict.fromkeys(prefixes))
    pages: list[str] = []
    if vault is not None:
        root = Path(vault) / "wiki"
        if root.exists():
            for p in sorted(root.rglob("*.md")):
                try:
                    text = p.read_text(encoding="utf-8")
                except (OSError, UnicodeDecodeError):
                    continue
                if any(pre in text for pre in prefixes):
                    pages.append(str(p.relative_to(Path(vault))))
        return pages
    rows = conn.execute(
        "SELECT DISTINCT doc_path FROM blocks WHERE layer='wiki' AND ("
        + " OR ".join("raw LIKE ?" for _ in prefixes)
        + ")",
        tuple(f"%{pre}%" for pre in prefixes),
    ).fetchall()
    return [r[0] for r in rows]


def get_block_states(conn: sqlite3.Connection, block_ids: list[str]) -> dict[str, str]:
    """Batch state lookup — one query instead of one-per-row.

    `filter_visible` runs on every retrieval; doing it row-by-row turned a single
    top-k search into N round-trips. SQLite has no unnest, so chunk into IN
    groups small enough to stay well under the bound-parameter limit.
    """
    ensure_tables(conn)
    out: dict[str, str] = {}
    ids = [b for b in block_ids if b]
    for i in range(0, len(ids), 400):
        chunk = ids[i : i + 400]
        marks = ",".join("?" * len(chunk))
        rows = conn.execute(
            f"SELECT block_id,state FROM block_state WHERE block_id IN ({marks})",
            chunk,
        ).fetchall()
        for r in rows:
            out[r[0]] = r[1]
    return out


def mark_stale(conn: sqlite3.Connection, page_path: str, reason: str = "") -> None:
    """Flag a page stale WITHOUT touching its lifecycle state (§8.3).

    `stale` and `state` are orthogonal axes: `stale` means "the compiled output
    is behind its sources", `state` carries the review verdict (disputed,
    retracted…). An earlier version wrote `state='active'` here, so cascading a
    retraction silently un-disputed every page it touched — fixing one error
    erased the record of another.
    """
    ensure_tables(conn)
    prev = conn.execute(
        "SELECT state FROM page_state WHERE page_path=?", (page_path,)
    ).fetchone()
    state = prev[0] if prev and prev[0] in VALID_STATES else "active"
    conn.execute(
        "INSERT OR REPLACE INTO page_state(page_path,stale,state,reason,updated)"
        " VALUES(?,1,?,?,?)",
        (page_path, state, reason, _now()),
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
    """Drop rows whose source block is superseded/retracted (§8.2).

    Batched: one lookup for the whole page of candidates. This is called on
    EVERY retrieval path (search / ask / MCP / eval), so the difference between
    N queries and 1 matters.
    """
    if not rows:
        return rows
    ensure_tables(conn)
    ids = [r.get("id") or r.get("block_id") for r in rows]
    states = get_block_states(conn, [i for i in ids if i])
    out = []
    for r, bid in zip(rows, ids):
        if bid and states.get(bid) in FILTERED:
            continue
        out.append(r)
    return out


# --- §8.3 query-time disposition (lazy synthesis update) -------------------


def page_dispositions(
    conn: sqlite3.Connection, page_paths: list[str]
) -> dict[str, str]:
    """Batch the §8.3 lookup — `disposition_for` is one row, queries see many."""
    ensure_tables(conn)
    out: dict[str, str] = {}
    paths = [p for p in page_paths if p]
    for i in range(0, len(paths), 400):
        chunk = paths[i : i + 400]
        marks = ",".join("?" * len(chunk))
        rows = conn.execute(
            f"SELECT page_path,stale,state FROM page_state WHERE page_path IN ({marks})",
            chunk,
        ).fetchall()
        for r in rows:
            out[r[0]] = _disposition(r[1], r[2])
    return out


def _disposition(stale: int, state: str, high_risk: bool = False) -> str:
    """The §8.3 table itself, shared by single and batch lookups."""
    if state == "retracted":
        return "hard_filter"
    if state == "superseded":
        return "filtered_unless_explicit_history"
    if stale:
        return "refuse_or_recompile" if high_risk else "downrank_and_async_recompile"
    return "use"


def apply_disposition(
    conn: sqlite3.Connection, rows: list[dict], high_risk: bool = False
) -> list[dict]:
    """§8.3 on the read path: stale pages sink, retracted pages vanish.

    `downrank_and_async_recompile` means literally that — evidence from a page
    whose sources moved on is pushed behind fresh evidence rather than being
    presented as equally current, and the recompile stays async (§5.3: compile
    never blocks availability).
    """
    if not rows:
        return rows
    paths = {r.get("doc_path") or "" for r in rows}
    disp = page_dispositions(conn, list(paths))
    keep, stale = [], []
    for r in rows:
        d = disp.get(r.get("doc_path") or "", "use")
        if d == "hard_filter":
            continue
        if d in {"downrank_and_async_recompile", "refuse_or_recompile"}:
            stale.append(r)
        else:
            keep.append(r)
    return keep + stale


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
    return _disposition(row[0], row[1], high_risk)
