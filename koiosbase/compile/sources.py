"""`sources/` guide pages — one per raw document (design doc §4.3, §5.1).

Each page is the *derived* projection of a source doc: a reading summary plus the
questions the document can answer. Those "answerable questions" double as an
intent-matching signal for the query router, but they are stored as plain
Markdown and rebuilt on ingest — never hand-maintained (P1).

v0.2 generates them deterministically from the document's own headings and
blocks, because the compile layer that would author them with an LLM lands in
v0.3. Generated pages are marked `generated: true` so a future LLM pass can tell
machine scaffolding from curated prose.
"""

from __future__ import annotations

import re
from pathlib import Path

from ..core.block import Block

PAGE_TEMPLATE = """---
type: source
title: {title}
source: raw/{rel}
confidence: draft
generated: true
last_compiled: {stamp}
---

# {title}

源文档：`raw/{rel}`

## 导读摘要
{summary}

## 本文档能回答的问题
{questions}

## 覆盖章节
{sections}
"""


def title_from_doc(doc) -> str:
    fm = doc.frontmatter or {}
    return fm.get("title") or Path(doc.path).stem


def build_summary(blocks: list[Block], max_chars: int = 280) -> str:
    """Opening content of the doc, compressed into a reading summary."""
    texts = [b.raw.replace("\n", " ").strip() for b in blocks if b.type == "paragraph"]
    if not texts:
        texts = [b.raw.replace("\n", " ").strip() for b in blocks]
    joined = " ".join(texts)
    sentences = re.split(r"(?<=[。！？；.!?;])\s*", joined)
    out = ""
    for s in sentences:
        if len(out) + len(s) > max_chars:
            break
        out += s + " "
    return (out or joined[:max_chars]).strip()


def build_questions(doc, blocks: list[Block], limit: int = 5) -> list[str]:
    """Questions this document answers, derived from its headings."""
    qs = []
    seen = set()
    for sec in doc.sections:
        t = (sec.title or "").strip()
        if not t or t.startswith(("__", "p.")):
            continue
        key = t.lower()
        if key in seen:
            continue
        seen.add(key)
        qs.append(f"关于「{t}」，本文档有哪些说明？ ({sec.id})")
        if len(qs) >= limit:
            break
    if not qs:
        for b in blocks[:limit]:
            qs.append(f"关于「{b.breadcrumb.split(' > ')[-1]}」，有哪些内容？ ({b.id})")
    return qs


def render_source_page(doc, blocks: list[Block], stamp: str) -> str:
    summary = build_summary(blocks)
    questions = "\n".join(f"- {q}" for q in build_questions(doc, blocks))
    sections = (
        "\n".join(
            f"- {s.title} (`{s.id}`)"
            for s in doc.sections
            if s.title and not s.title.startswith("__")
        )
        or "- (无显式标题)"
    )
    return PAGE_TEMPLATE.format(
        title=title_from_doc(doc),
        rel=doc.path,
        stamp=stamp,
        summary=summary,
        questions=questions,
        sections=sections,
    )


def write_source_pages(vault: Path, docs_and_blocks: list, stamp: str) -> int:
    """Write wiki/sources/<name>.md for each source doc. Returns count written."""
    out_dir = vault / "wiki" / "sources"
    out_dir.mkdir(parents=True, exist_ok=True)
    n = 0
    for doc, blocks in docs_and_blocks:
        if not blocks:
            continue
        name = Path(doc.path).stem
        target = out_dir / f"{name}.md"
        target.write_text(render_source_page(doc, blocks, stamp), encoding="utf-8")
        n += 1
    return n
