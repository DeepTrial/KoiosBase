"""ACL / multi-tenancy (design doc §9.2).

The rule that matters: **filter at retrieval time, not after generation.**
Masking an answer that has already been composed leaks in paraphrase — a model
asked to "omit X" still carries X's influence. So the ACL check happens in the
recall path, before a block can ever reach the context window.

ACLs are declared in frontmatter (`acl: [finance-team]`) — Markdown stays the
source of truth (P1) and permissions are versioned with the content.
"""

from __future__ import annotations

import json
import sqlite3
from collections.abc import Callable

# An empty/absent acl means unrestricted.
PUBLIC = None


def grants_for(conn: sqlite3.Connection, doc_path: str) -> list[str] | None:
    """Read a document's acl from its frontmatter (None = public)."""
    row = conn.execute(
        "SELECT frontmatter FROM documents WHERE path=?", (doc_path,)
    ).fetchone()
    if not row:
        return PUBLIC
    try:
        fm = json.loads(row[0] or "{}")
    except json.JSONDecodeError:
        return PUBLIC
    acl = fm.get("acl")
    if not acl:
        return PUBLIC
    if isinstance(acl, str):
        acl = [acl]
    return [str(a) for a in acl]


def allowed(acl: list[str] | None, principal_groups: set[str] | None) -> bool:
    """True if the principal may see this document."""
    if not acl:
        return True  # public document
    if not principal_groups:
        return False  # restricted doc, anonymous caller
    return bool(set(acl) & principal_groups)


def filter_rows(
    conn: sqlite3.Connection, rows: list[dict], principal_groups: set[str] | None
) -> list[dict]:
    """Drop rows the principal is not entitled to — before context assembly."""
    if principal_groups is None and not _any_restricted(conn):
        return rows
    out = []
    cache: dict[str, list[str] | None] = {}
    for r in rows:
        dp = r.get("doc_path") or ""
        if dp not in cache:
            cache[dp] = grants_for(conn, dp)
        if allowed(cache[dp], principal_groups):
            out.append(r)
    return out


def _any_restricted(conn: sqlite3.Connection) -> bool:
    """Cheap pre-check: does any document declare an acl at all?"""
    rows = conn.execute("SELECT frontmatter FROM documents").fetchall()
    for r in rows:
        try:
            fm = json.loads(r[0] or "{}")
        except json.JSONDecodeError:
            continue
        if fm.get("acl"):
            return True
    return False


def visible_paths(
    conn: sqlite3.Connection, principal_groups: set[str] | None
) -> list[str]:
    """Which documents this principal can see (used by Studio exports).

    A section title is content too, so tree/mindmap views filter by document
    rather than by block text.
    """
    out = []
    cache: dict[str, list[str] | None] = {}
    for r in conn.execute("SELECT path FROM documents").fetchall():
        path = r[0]
        if path not in cache:
            cache[path] = grants_for(conn, path)
        if allowed(cache[path], principal_groups):
            out.append(path)
    return out


def visible_doc_paths(
    conn: sqlite3.Connection, principal_groups: set[str] | None
) -> Callable[[str], bool]:
    """Membership predicate over visible documents, read ONCE (§9.2).

    A closure so a caller filtering thousands of rows does one pass over
    `documents` instead of one lookup per row. Anonymous callers over a corpus
    with no acl anywhere short-circuit to everything-visible — the common case,
    and `filter_rows` already established that _any_restricted must stay cheap.
    """
    if principal_groups is None and not _any_restricted(conn):
        return lambda _path: True
    seen = set(visible_paths(conn, principal_groups))
    return lambda path: path in seen
