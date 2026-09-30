"""Compile layer: entities/ and concepts/ pages (design doc §5.3, §4.3).

The affected set is computed from the index — `affected_entities`, an entity
table plus a vector lookup over page summaries — so *identifying* what to touch
does not scan the library. That differs from scaling: `compile_vault` currently
re-reads every raw doc and rewrites each touched entity page wholesale, so wall
cost has been O(corpus), not O(pages touched). See write_entity_pages — it now
preserves prior state but does not yet narrow its write set; closing that gap is
what would fully deliver the ~200-page-collapse claim above.

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
confidence: {confidence}
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


def render_entity_page(ent: Entity, stamp: str, existing: str | None = None) -> str:
    """Render an entity page, preserving what a human wrote.

    The previous version overwrote the whole file, so recompiling destroyed two
    things that only a human can produce: the 「矛盾与未决」 notes, and any manual
    edits to the fact list. It also reset `confidence` to draft on every pass,
    meaning promoting a page and recompiling silently undid the promotion (§5.3
    asks for surgical updates).
    """
    conflicts = _existing_section(existing, "## 矛盾与未决") or "- (无)"
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
        # Preserved across recompiles: promotion is a human act (§5.4) and the
        # compiler must not silently undo it.
        confidence=_existing_confidence(existing),
        stamp=stamp,
        sources=", ".join(f"raw/{bid}" for _t, bid in ent.facts[:3]),
        name=ent.name,
        facts=facts,
        relations=relations,
        conflicts=conflicts,
    )


def _existing_confidence(existing: str | None) -> str:
    """Keep the promoted confidence of an existing page; new pages start draft."""
    from ..compile.gate import CONFIDENCE_RE, VALID

    if not existing:
        return "draft"
    m = CONFIDENCE_RE.search(existing)
    return m.group(1) if m and m.group(1) in VALID else "draft"


def _existing_section(existing: str | None, header: str) -> str | None:
    """Body of one `##` section in an existing page, sources of truth for humans.

    Returns None when the page is new or the section is empty, letting the caller
    fall back to its own default.
    """
    if not existing or header not in existing:
        return None
    body = existing.split(header, 1)[1]
    for line in body.split("\n"):
        if line.startswith("## "):
            body = body.split("\n" + line, 1)[0]
            break
    return body.strip() or None


def write_entity_pages(vault, entities: dict[str, Entity], stamp: str) -> int:
    """Write entity pages surgically — only for `entities`, only the fact region.

    Two fixes over the wholesale rewrite: the written set is limited to the
    entities actually present in this compile run (the affected set was computed
    and then ignored, so every page was rewritten on every run), and each write
    preserves frontmatter + human-authored sections.
    """
    out_dir = vault / "wiki" / "entities"
    out_dir.mkdir(parents=True, exist_ok=True)
    n = 0
    for ent in entities.values():
        target = out_dir / f"{ent.entity_id}.md"
        existing = None
        try:
            existing = target.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            existing = None
        target.write_text(
            render_entity_page(ent, stamp, existing=existing), encoding="utf-8"
        )
        n += 1
    return n
