"""Studio-style outputs (design doc §13 v1.0): brief / FAQ / mindmap.

These are *projections* of knowledge that already exists — they compile nothing
new and add no assertions of their own. Every generated statement carries the
citation it came from (P6 applies to exports just as much as to answers), and an
export with no evidence says so rather than inventing filler.
"""

from __future__ import annotations

import re
import sqlite3
from datetime import datetime, timezone
from pathlib import Path

BRIEF_TEMPLATE = """---
type: brief
generated: true
created: {stamp}
sources: [{sources}]
---

# {title}

> 由 KoiosBase 生成 · 共 {n} 条证据 · 生成时间 {stamp}

## 要点
{bullets}

## 证据清单
{evidence}
"""

FAQ_TEMPLATE = """---
type: faq
generated: true
created: {stamp}
---

# {title}

{items}
"""

MINDMAP_TEMPLATE = """---
type: mindmap
generated: true
created: {stamp}
---

# {title}

```mermaid
mindmap
  root(({root}))
{branches}
```
"""


def _sentences(text: str, limit: int = 3) -> list[str]:
    parts = re.split(r"(?<=[。！？；.!?;])\s*", (text or "").replace("\n", " "))
    return [p.strip() for p in parts if p.strip()][:limit]


def render_brief(title: str, blocks: list[dict], stamp: str) -> str:
    bullets = []
    seen: set[str] = set()
    for b in blocks:
        for s in _sentences(b.get("raw", ""), limit=2):
            key = s[:40]
            if key in seen:
                continue
            seen.add(key)
            bullets.append(f"- {s} [[raw/{b['id']}]]")
    evidence = "\n".join(
        f"- `{b['id']}` — {b.get('breadcrumb', '')}" for b in blocks[:20]
    )
    return BRIEF_TEMPLATE.format(
        stamp=stamp,
        title=title,
        n=len(blocks),
        bullets="\n".join(bullets[:12]) or "- (无证据)",
        evidence=evidence or "- (无)",
        sources=", ".join(f"raw/{b['id']}" for b in blocks[:5]),
    )


def render_faq(title: str, qa: list[tuple[str, str, list[dict]]], stamp: str) -> str:
    items = []
    for q, ans, ev in qa:
        cites = " ".join(f"[[raw/{b['id']}]]" for b in ev[:3])
        items.append(f"## {q}\n\n{ans}\n\n依据：{cites or '（无）'}\n")
    return FAQ_TEMPLATE.format(
        stamp=stamp, title=title, items="\n".join(items) or "（无问答）"
    )


def render_mindmap(
    title: str, root: str, branches: list[tuple[str, list[str]]], stamp: str
) -> str:
    lines = []
    for head, leaves in branches[:12]:
        safe = re.sub(r"[()\[\]]", " ", head).strip() or "topic"
        lines.append(f"  {safe}")
        for leaf in leaves[:6]:
            lsafe = re.sub(r"[()\[\]]", " ", leaf).strip()
            if lsafe:
                lines.append(f"    {lsafe}")
    return MINDMAP_TEMPLATE.format(
        stamp=stamp,
        title=title,
        root=re.sub(r"[()\[\]]", " ", root).strip() or root,
        branches="\n".join(lines) or "  (空)",
    )


def _load_blocks(
    conn: sqlite3.Connection, bids: list[str]
) -> list[dict]:
    """Fetch full rows for ids, dropping any that vanished since scoring."""
    out = []
    for bid in bids:
        row = conn.execute("SELECT * FROM blocks WHERE id=?", (bid,)).fetchone()
        if row:
            out.append(dict(row))
    return out


def studio_brief(
    conn: sqlite3.Connection,
    vault: str | Path,
    topic: str,
    top: int = 8,
    principal_groups: set[str] | None = None,
) -> Path:
    """Export a brief — filtered by ACL, like every other read path (§9.2).

    Exports are the worst place to leak: unlike a chat answer, a brief is written
    to disk and outlives the request, so a restricted sentence in an export is a
    persisted breach rather than a transient one. Same three filters as
    `retrieve()` — ACL, then retracted, then stale disposition.
    """
    from ..retrieval.router import hybrid_search
    from ..security.acl import filter_rows
    from ..state.model import apply_disposition, filter_visible

    vault = Path(vault)
    rows = hybrid_search(conn, topic, limit=top)
    blocks = _load_blocks(conn, [bid for bid, _s in rows])
    blocks = filter_rows(conn, blocks, principal_groups)
    blocks = filter_visible(blocks, conn)
    blocks = apply_disposition(conn, blocks)
    out = vault / "wiki" / "synthesis"
    out.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).date().isoformat()
    slug = re.sub(r"[^\w\u4e00-\u9fff]+", "-", topic)[:40] or "brief"
    target = out / f"brief-{slug}.md"
    target.write_text(render_brief(topic, blocks, stamp), encoding="utf-8")
    return target


def studio_mindmap(
    conn: sqlite3.Connection,
    vault: str | Path,
    root: str,
    principal_groups: set[str] | None = None,
) -> Path:
    """Build a mindmap from the section tree — structure, not new claims.

    A section TITLE is content too: a page named 「收购对价细节」 leaks that the
    acquisition exists even before you read a sentence. So titles are filtered
    with the same ACL as text (§9.2) — `visible_paths` was written for exactly
    this but nothing called it.
    """
    from ..security.acl import visible_doc_paths

    vault = Path(vault)
    visible = visible_doc_paths(conn, principal_groups)
    rows = conn.execute(
        "SELECT doc_path,title,parent_id FROM sections ORDER BY doc_path, ordinal"
    ).fetchall()
    rows = [r for r in rows if visible(r["doc_path"])]
    by_parent: dict[str, list[str]] = {}
    for r in rows:
        key = r["parent_id"] or r["doc_path"]
        by_parent.setdefault(key, []).append(r["title"])
    branches = [(k, v) for k, v in list(by_parent.items())[:12]]
    out = vault / "wiki" / "synthesis"
    out.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).date().isoformat()
    slug = re.sub(r"[^\w\u4e00-\u9fff]+", "-", root)[:40] or "map"
    target = out / f"mindmap-{slug}.md"
    target.write_text(render_mindmap(root, root, branches, stamp), encoding="utf-8")
    return target
