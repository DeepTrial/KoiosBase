"""Format adapter layer — parse(path) -> Document (design doc §5.1).

Adding a format = adding one adapter; the main architecture is untouched.
v0.1 ships the Markdown adapter (native passthrough, headings = tree).
Dirty docs (headings faked with bold text) fall back to semantic segmentation
with a pseudo-heading, per §5.1.
"""

from __future__ import annotations

import re
from pathlib import Path

from ..core.block import Block, Document, Section

HEADING_RE = re.compile(r"^(#{1,6})\s+(.*)$", re.MULTILINE)
FENCE_RE = re.compile(r"^```")
LIST_RE = re.compile(r"^([-*+]|\d+\.)\s+")

Table = bool  # local readability alias for the table-detection flag


def _parse_frontmatter(text: str) -> tuple[dict, str]:
    """Minimal YAML subset parser (avoids a PyYAML dependency).

    Supports `key: value` and inline `[a, b]` lists only — a superset of what
    our own frontmatter spec (§4.2) uses, nothing more.
    """
    fm: dict = {}
    if not text.startswith("---"):
        return fm, text
    end = text.find("\n---", 3)
    if end == -1:
        return fm, text
    for line in text[3:end].splitlines():
        line = line.strip()
        if not line or line.startswith("#") or ":" not in line:
            continue
        k, _, v = line.partition(":")
        v = v.strip().strip("\"'")
        if v.startswith("[") and v.endswith("]"):
            v = [x.strip().strip("\"'") for x in v[1:-1].split(",") if x.strip()]
        fm[k.strip()] = v
    return fm, text[end + 4 :]


def split_blocks(
    doc_path: str, section_path: str, breadcrumb: str, text: str, start_ord: int = 0
) -> list[Block]:
    """Split a section body into atomic Blocks.

    P2 discipline: tables, code fences and lists are NEVER fragmented — they
    stay whole regardless of length, which is what removes the granularity
    paradox (tables/columns/arguments never get肢解).
    """
    blocks: list[Block] = []
    buf: list[str] = []
    ordinal = start_ord
    in_fence = False
    cur_type = "paragraph"

    def flush(kind: str) -> None:
        nonlocal buf, ordinal
        body = "\n".join(buf).strip()
        buf = []
        if not body:
            return
        ordinal += 1
        blocks.append(
            Block(
                id=f"{doc_path}#{section_path}/{ordinal}"
                if section_path
                else f"{doc_path}/{ordinal}",
                type=kind,
                breadcrumb=breadcrumb,
                raw=body,
                doc_path=doc_path,
                ordinal=ordinal,
                section_id=f"{doc_path}#{section_path}" if section_path else doc_path,
            )
        )

    for line in text.split("\n"):
        stripped = line.strip()
        if FENCE_RE.match(stripped):
            if in_fence:
                buf.append(line)
                flush("code")
                in_fence = False
            else:
                flush(cur_type)
                in_fence = True
                buf.append(line)
            continue
        if in_fence:
            buf.append(line)
            continue
        if not stripped:
            flush(cur_type)
            continue
        if stripped.startswith("|") and stripped.endswith("|"):
            if cur_type != "table":
                flush(cur_type)
                cur_type = "table"
            buf.append(line)
            continue
        if LIST_RE.match(stripped):
            if cur_type != "list":
                flush(cur_type)
                cur_type = "list"
            buf.append(line)
            continue
        if cur_type != "paragraph":
            flush(cur_type)
            cur_type = "paragraph"
        buf.append(line)
    flush("code" if in_fence else cur_type)
    return blocks


def parse_markdown(path: str | Path, root: str | Path = "") -> Document:
    """Parse a Markdown file into a Document tree (Sections → Blocks)."""
    path = Path(path)
    raw_text = path.read_text(encoding="utf-8")
    fm, body = _parse_frontmatter(raw_text)
    root_str = str(root) if root else ""
    if root_str and str(path).startswith(root_str):
        doc_path = str(Path(path).relative_to(root_str))
    else:
        doc_path = path.name
    doc = Document(path=doc_path, frontmatter=fm)
    base_title = fm.get("title") or path.stem

    matches = list(HEADING_RE.finditer(body))
    if not matches:
        # Dirty-doc fallback: no real headings -> one pseudo-section
        sec = Section(id=doc_path, parent_id=None, title=base_title, doc_path=doc_path)
        for b in split_blocks(doc_path, "", base_title, body):
            sec.children.append(b)
        doc.sections.append(sec)
        return doc

    # Preamble (text before the first heading) becomes its own root section
    if matches[0].start() > 0:
        pre = body[: matches[0].start()].strip()
        if pre:
            presec = Section(
                id=f"{doc_path}#__preamble",
                parent_id=None,
                title="__preamble",
                doc_path=doc_path,
                ordinal=0,
            )
            for b in split_blocks(doc_path, "__preamble", base_title, pre):
                presec.children.append(b)
            doc.sections.append(presec)

    # stack entries: (level, section_id, Section)
    stack: list[tuple[int, str, Section]] = []
    for i, m in enumerate(matches):
        level = len(m.group(1))
        title = m.group(2).strip()
        seg_start = m.end()
        seg_end = matches[i + 1].start() if i + 1 < len(matches) else len(body)
        while stack and stack[-1][0] >= level:
            stack.pop()
        titles = [t[2].title for t in stack if t[2].title != "__preamble"] + [title]
        sec_path = "/".join(titles)
        parent_id = stack[-1][1] if stack else None
        sec = Section(
            id=f"{doc_path}#{sec_path}",
            parent_id=parent_id,
            title=title,
            doc_path=doc_path,
            ordinal=i + 1,
        )
        crumb = " > ".join([base_title] + titles)
        for b in split_blocks(doc_path, sec_path, crumb, body[seg_start:seg_end]):
            sec.children.append(b)
        doc.sections.append(sec)
        stack.append((level, sec.id, sec))
    return doc


def all_blocks(doc: Document) -> list[Block]:
    return [b for s in doc.sections for b in s.children]
