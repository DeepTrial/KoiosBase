"""Core data types: Block, Section, Document (see design doc §4.1).

P1 — Markdown is the single source of truth: every field here is derivable
from the raw Markdown; nothing is stored that cannot be recomputed.
P2 — Block is the only storage unit; coarser granularities are query-time views.
P6 — every Block carries a stable, human-readable id usable as a citation.
"""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass, field


def content_hash(text: str) -> str:
    """Stable hash of normalized text — drives incremental re-indexing."""
    norm = re.sub(r"\s+", " ", text or "").strip()
    return hashlib.sha256(norm.encode("utf-8")).hexdigest()[:16]


@dataclass
class Block:
    """Semantic atomic unit — the only storage unit in KoiosBase."""

    id: str  # "财务/2024年报.md#负债分析/块3"
    type: str  # paragraph | table | code | list | figure
    breadcrumb: str  # "2024年报 > 财务分析 > 负债分析"
    raw: str
    doc_path: str
    ordinal: int = 0
    section_id: str | None = None
    page_range: Optional[list] = None  # PDF only — page-level provenance
    hash: str = ""
    meta: dict = field(default_factory=dict)

    def __post_init__(self):
        if not self.hash:
            self.hash = content_hash(self.breadcrumb + "\n" + self.raw)

    @property
    def embedded_text(self) -> str:
        """Text that participates in embedding — breadcrumb injected to resolve
        dangling references (Contextual Retrieval, zero LLM cost)."""
        return f"{self.breadcrumb}\n\n{self.raw}" if self.breadcrumb else self.raw

    def citation(self) -> str:
        if self.page_range:
            return f"{self.id} (p.{self.page_range[0]}-{self.page_range[1]})"
        return self.id


@dataclass
class Section:
    """Node of the document tree. summary is a *navigational* summary, not prose."""

    id: str
    parent_id: str | None
    title: str
    summary: str = ""
    doc_path: str = ""
    ordinal: int = 0
    children: list = field(default_factory=list)


@dataclass
class Document:
    path: str
    frontmatter: dict = field(default_factory=dict)
    sections: list = field(default_factory=list)

    @property
    def title(self) -> str:
        return self.frontmatter.get("title") or self.path.rsplit("/", 1)[-1]
