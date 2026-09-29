"""lint gardener (design doc §9.3) — incremental + sampled, never global sweeps.

v0.1 ships the L1 programmatic checks (§10.3): those need no model at all and
therefore carry zero self-verification risk (P7 prefers the lowest level that
can do the job).
"""

from __future__ import annotations

import re
import sqlite3
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


def run_l1(conn: sqlite3.Connection) -> dict[str, list[str]]:
    return {
        "broken_links": check_broken_links(conn),
        "unsourced_assertions": check_unsourced_assertions(conn),
    }


def cmd_lint(args=None) -> int:
    vault = Path(getattr(args, "path", ".")).resolve()
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
