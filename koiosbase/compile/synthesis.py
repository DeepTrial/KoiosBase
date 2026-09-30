"""synthesis/ pages with lazy update (design doc §4.3, §8.3).

Synthesis pages are the most expensive to compile and depend on entity pages
being mature, so they are NOT rebuilt eagerly: a source change marks them
`stale`, and the actual recompilation is deferred until a query touches them.
Recompiling on every ingest would spend the cost whether or not anyone reads the
page; deferring also keeps ingest fast (§5.3: compile never blocks availability).
"""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path

from ..state.model import ensure_tables, is_stale, mark_stale

SYNTHESIS_TEMPLATE = """---
type: synthesis
confidence: draft
generated: true
last_compiled: {stamp}
stale: {stale}
sources: [{sources}]
---

# {title}

## 综合结论
{body}

## 覆盖实体
{entities}
"""


def stamp_now() -> str:
    return datetime.now(timezone.utc).date().isoformat()


def render_synthesis(
    title: str,
    entities: list[str],
    facts: list[tuple[str, str]],
    stamp: str,
    stale: bool = False,
) -> str:
    body = "\n".join(f"- {text} [[raw/{bid}]]" for text, bid in facts[:8]) or "- (暂无)"
    return SYNTHESIS_TEMPLATE.format(
        stamp=stamp,
        stale=str(stale).lower(),
        title=title,
        sources=", ".join(f"raw/{bid}" for _t, bid in facts[:3]),
        body=body,
        entities="\n".join(f"- [[entities/{e}]]" for e in entities[:10]) or "- (暂无)",
    )


def mark_syntheses_stale(
    conn, vault: str | Path, topics: list[str] | None = None
) -> list[str]:
    """Flag synthesis pages stale (called when sources change).

    `topics` narrows the blast radius to the pages that actually depend on the
    changed source. Without it every synthesis page is flagged, so touching one
    raw doc forced a full recompile of the most expensive layer on next read —
    which is exactly what lazy update (§8.3) exists to avoid. Callers that pass
    nothing keep the previous behaviour.
    """
    vault = Path(vault)
    sdir = vault / "wiki" / "synthesis"
    if not sdir.exists():
        return []
    ensure_tables(conn)
    wanted = None if topics is None else {t.strip() for t in topics if t and t.strip()}
    touched = []
    for p in sorted(sdir.glob("*.md")):
        if wanted is not None and p.stem not in wanted:
            continue
        rel = f"wiki/synthesis/{p.name}"
        mark_stale(conn, rel, "source changed")
        touched.append(rel)
    conn.commit()
    return touched


def needs_recompile(conn, vault: str | Path, topic: str) -> bool:
    """True if the topic's synthesis page is stale or missing (§8.3 lazy)."""
    vault = Path(vault)
    target = vault / "wiki" / "synthesis" / f"{topic}.md"
    if not target.exists():
        return True
    return is_stale(conn, f"wiki/synthesis/{target.name}")


def recompile_synthesis(
    conn,
    vault: str | Path,
    topic: str,
    entities: list[str],
    facts: list[tuple[str, str]],
) -> Path:
    """(Re)build one synthesis page and clear its stale flag."""
    vault = Path(vault)
    sdir = vault / "wiki" / "synthesis"
    sdir.mkdir(parents=True, exist_ok=True)
    target = sdir / f"{topic}.md"
    target.write_text(
        render_synthesis(topic, entities, facts, stamp_now(), stale=False),
        encoding="utf-8",
    )
    ensure_tables(conn)
    conn.execute(
        "INSERT OR REPLACE INTO page_state(page_path,stale,state,reason,updated)"
        " VALUES(?,0,'active','recompiled',?)",
        (
            f"wiki/synthesis/{target.name}",
            datetime.now(timezone.utc).isoformat(timespec="seconds"),
        ),
    )
    conn.commit()
    return target
