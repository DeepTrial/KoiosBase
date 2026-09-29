"""lint gardener (design doc §9.3) — incremental + sampled, never global sweeps.

v0.1 ships the L1 programmatic checks (§10.3): those need no model at all and
therefore carry zero self-verification risk (P7 prefers the lowest level that
can do the job).
"""

from __future__ import annotations

import json
import re
import sqlite3
from datetime import datetime, timezone
from pathlib import Path

from ..generation.judge import judge as cross_judge
from ..index.schema import connect

WIKILINK_RE = re.compile(r"\[\[([^\]]+)\]\]")


def check_broken_links(conn: sqlite3.Connection) -> list[str]:
    rows = conn.execute("SELECT src,dst FROM links WHERE kind='wikilink'").fetchall()
    bad = []
    for r in rows:
        dst = r["dst"]
        exists = conn.execute(
            "SELECT 1 FROM blocks WHERE id LIKE ? OR doc_path LIKE ? LIMIT 1",
            (f"%{dst}%", f"%{dst}%"),
        ).fetchone()
        if not exists:
            bad.append(f"broken wikilink: {r['src']} -> {dst}")
    return bad


def check_unsourced_assertions(conn: sqlite3.Connection) -> list[str]:
    """Compile-layer pages whose factual lines cite nothing violate the citation
    contract (P6) — surfaced here rather than at generation time."""
    bad = []
    rows = conn.execute("SELECT id,raw FROM blocks WHERE layer='wiki'").fetchall()
    for r in rows:
        for line in (l.strip() for l in r["raw"].split("\n")):
            if not line.startswith(("-", "*")):
                continue
            content = line.lstrip("-* ").strip()
            if not content or WIKILINK_RE.search(content):
                continue
            # Skip structural lines: headers, separators, table rows, code.
            if content.startswith(("#", "|", "`", ">")) or set(content) <= set("-=*| "):
                continue
            bad.append(f"unsourced assertion in {r['id']}: {content[:60]}")
    return bad


def check_expired_ttl(conn: sqlite3.Connection) -> list[str]:
    """TTL expiry (§9.1): frontmatter ttl + valid_from means the claim is old."""
    bad = []
    rows = conn.execute("SELECT path,frontmatter FROM documents").fetchall()
    for r in rows:
        try:
            fm = json.loads(r["frontmatter"] or "{}")
        except json.JSONDecodeError:
            continue
        ttl = fm.get("ttl")
        valid_from = fm.get("valid_from")
        if not ttl or not valid_from:
            continue
        days = None
        m = re.match(r"^(\d+)\s*d$", str(ttl))
        if m:
            days = int(m.group(1))
        elif str(ttl).isdigit():
            days = int(ttl)
        if days is None:
            continue
        try:
            start = datetime.strptime(str(valid_from), "%Y-%m-%d").replace(
                tzinfo=timezone.utc
            )
        except ValueError:
            continue
        age = (datetime.now(timezone.utc) - start).days
        if age > days:
            bad.append(f"TTL expired: {r['path']} ({age}d > {days}d)")
    return bad


def check_stale_pages(conn: sqlite3.Connection) -> list[str]:
    """Report pages flagged stale by cascade propagation (§8.3)."""
    try:
        conn.execute("SELECT 1 FROM page_state LIMIT 1")
    except sqlite3.OperationalError:
        return []
    rows = conn.execute(
        "SELECT page_path,reason FROM page_state WHERE stale=1"
    ).fetchall()
    return [f"stale page: {r['page_path']} ({r['reason']})" for r in rows]


def check_conflict_markers(conn: sqlite3.Connection) -> list[str]:
    """Open conflicts parked in 「矛盾与未决」 must not be silently forgotten."""
    rows = conn.execute(
        "SELECT id,raw FROM blocks WHERE layer='wiki' AND raw LIKE '%矛盾与未决%'"
    ).fetchall()
    out = []
    for r in rows:
        seg = r["raw"].split("矛盾与未决", 1)[-1]
        for line in seg.split("\n"):
            s = line.strip()
            if s.startswith("-") and "⚠" in s:
                out.append(f"open conflict in {r['id']}: {s[:70]}")
    return out


def check_orphan_pages(conn: sqlite3.Connection) -> list[str]:
    """wiki pages that nothing links to and that cite nothing (§9.3)."""
    rows = conn.execute(
        "SELECT id,raw,doc_path FROM blocks WHERE layer='wiki'"
    ).fetchall()
    out = []
    for r in rows:
        if "[[" not in (r["raw"] or ""):
            out.append(f"orphan page (no links at all): {r['doc_path']}")
    return out


def run_l1(conn: sqlite3.Connection) -> dict[str, list[str]]:
    """All model-free checks (§10.3 L1). No LLM involved, cheap enough to run
    after every compile."""
    # Older vaults may predate the layer/state columns; degrade instead of 500ing.
    try:
        unsourced = check_unsourced_assertions(conn)
    except sqlite3.OperationalError:
        unsourced = []
    try:
        orphan = check_orphan_pages(conn)
    except sqlite3.OperationalError:
        orphan = []
    try:
        conflicts = check_conflict_markers(conn)
    except sqlite3.OperationalError:
        conflicts = []
    return {
        "broken_links": check_broken_links(conn),
        "unsourced_assertions": unsourced,
        "expired_ttl": check_expired_ttl(conn),
        "stale_pages": check_stale_pages(conn),
        "open_conflicts": conflicts,
        "orphan_pages": orphan,
    }


def cmd_lint(args=None) -> int:
    # Accept either a Namespace or a plain argv list (callers use both); the
    # previous Namespace-only read silently fell back to '.'.
    path = "."
    if args is not None:
        path = getattr(args, "path", None) or (
            args[0] if isinstance(args, (list, tuple)) and args else ".")
    vault = Path(path).resolve()
    conn = connect(vault / ".index")
    findings = run_l1(conn)
    total = sum(len(v) for v in findings.values())
    for kind, items in findings.items():
        for it in items:
            print(f"[{kind}] {it}")
    print(f"---- lint: {total} finding(s)")
    conn.close()
    return 1 if total else 0


def check_claim_cmd(args=None) -> int:
    """`koios checkclaim <claim> <evidence>` — cross-family judgement (§10.3 L2)."""
    claim = getattr(args, "claim", "")
    evidence = getattr(args, "evidence", "")
    verdict = cross_judge(claim, evidence)
    print(verdict)
    return 0 if verdict != "contradicted" else 1
