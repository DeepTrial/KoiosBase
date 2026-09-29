"""Compile layer: entities/ and concepts/ pages (design doc §5.3, §4.3).

The defining property of this layer is that **the compiler never scans the whole
library**. The affected set is computed from the index (entity table + a vector
lookup over page summaries), so cost is O(pages touched), not O(corpus size) —
this is what removes the ~200-page collapse reported for the LLM-Wiki pattern.

Compiled pages are Markdown truth (they live in wiki/ and are versioned), but
every fact they assert must carry a wikilink back to raw (P6). Pages start at
`confidence: draft` and are promoted only via cross-family verification or human
confirmation — never by citation counts (§5.4).
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

from ..core.block import Block

# Latin proper nouns: a capitalised run of 2+ tokens, or a sole all-caps token
# (ACME, IBM). Single ordinary capitalised words (Revenue, Total, Operating) are
# NOT entities — treating sentence-initial words as entities produced junk pages.
ENTITY_PATTERNS = [
    re.compile(r"\b([A-Z][A-Za-z0-9]*(?:\s+[A-Z][A-Za-z0-9]*)+)\b"),
    re.compile(r"\b([A-Z]{2,}[A-Za-z0-9]*)\b"),
    re.compile(r"([\u4e00-\u9fff]{2,8}(?:公司|集团|银行|研究院|大学|中心|部))"),
]
STOPWORDS = {
    "The",
    "This",
    "That",
    "However",
    "Therefore",
    "In",
    "On",
    "For",
    "Note",
    "Total",
    "Revenue",
    "Operating",
    "Cash",
    "Debt",
    "Net",
    "Gross",
    "Annual",
    "Financial",
    "Report",
    "Page",
}


@dataclass
class Entity:
    entity_id: str
    name: str
    aliases: list = field(default_factory=list)
    facts: list = field(default_factory=list)  # (text, block_id)
    relations: list = field(default_factory=list)  # (predicate, target)


def slugify(name: str) -> str:
    s = re.sub(r"[^\w\u4e00-\u9fff]+", "-", name.strip()).strip("-").lower()
    return s or "entity"


def extract_entities(blocks: list[Block]) -> dict[str, Entity]:
    """Cheap deterministic entity extraction (no LLM in the default path)."""
    found: dict[str, Entity] = {}
    for b in blocks:
        for pat in ENTITY_PATTERNS:
            for m in pat.findall(b.raw):
                name = m if isinstance(m, str) else m[0]
                name = name.strip()
                if not name or name in STOPWORDS or len(name) < 2:
                    continue
                eid = slugify(name)
                ent = found.setdefault(eid, Entity(entity_id=eid, name=name))
                if name not in ent.aliases and name != ent.name:
                    ent.aliases.append(name)
                snippet = b.raw.strip().replace("\n", " ")
                if len(snippet) > 160:
                    snippet = snippet[:160] + "…"
                ent.facts.append((snippet, b.id))
    return found


def affected_entities(conn, entities: dict[str, Entity]) -> list[str]:
    """Which entity pages already exist for these entities (indexed lookup)."""
    out = []
    for eid in entities:
        row = conn.execute(
            "SELECT 1 FROM blocks WHERE doc_path LIKE ? LIMIT 1", (f"%{eid}.md",)
        ).fetchone()
        if row:
            out.append(eid)
    return out


def affected_concepts(conn, blocks: list[Block], top_k: int = 10) -> list[str]:
    """Which concept/synthesis pages are near these blocks (summary lookup).

    Stands in for a vector search over page summaries — deterministic in v0.3 so
    the affected-set computation is testable without embeddings.
    """
    terms = set()
    for b in blocks:
        terms.update(re.findall(r"[A-Za-z0-9\u4e00-\u9fff]{3,}", b.raw.lower()))
    if not terms:
        return []
    rows = conn.execute(
        "SELECT doc_path, summary FROM sections WHERE summary IS NOT NULL AND summary != ''"
    ).fetchall()
    scored = []
    for r in rows:
        hay = (r["summary"] or "").lower()
        hits = sum(1 for t in terms if t in hay)
        if hits:
            scored.append((r["doc_path"], hits))
    scored.sort(key=lambda x: -x[1])
    return [d for d, _ in scored[:top_k]]


ENTITY_TEMPLATE = """---
type: entity
entity_id: {eid}
aliases: [{aliases}]
confidence: draft
generated: true
last_compiled: {stamp}
sources: [{sources}]
---

# {name}

## 关键事实
{facts}

## 关系
{relations}

## 矛盾与未决
{conflicts}
"""


def render_entity_page(ent: Entity, stamp: str) -> str:
    facts = (
        "\n".join(f"- {text} [[raw/{bid}]]" for text, bid in ent.facts[:8])
        or "- (暂无)"
    )
    relations = (
        "\n".join(f"- related → [[entities/{eid}]]" for _p, eid in ent.relations[:5])
        or "- (暂无)"
    )
    return ENTITY_TEMPLATE.format(
        eid=ent.entity_id,
        aliases=", ".join(ent.aliases[:5]),
        stamp=stamp,
        sources=", ".join(f"raw/{bid}" for _t, bid in ent.facts[:3]),
        name=ent.name,
        facts=facts,
        relations=relations,
        conflicts="- (无)",
    )


def write_entity_pages(vault, entities: dict[str, Entity], stamp: str) -> int:
    out_dir = vault / "wiki" / "entities"
    out_dir.mkdir(parents=True, exist_ok=True)
    n = 0
    for ent in entities.values():
        (out_dir / f"{ent.entity_id}.md").write_text(
            render_entity_page(ent, stamp), encoding="utf-8"
        )
        n += 1
    return n
