"""Ingest pipeline: raw/ + wiki/ → .index/ (design doc §5).

Synchronous indexing only in v0.1; the compile stage (entities/concepts) lands
in v0.3 (§13). Indexing is idempotent: content hashes drive incremental updates.
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path

from ..index.schema import (
    add_link,
    connect,
    upsert_document,
    upsert_sections_and_blocks,
)
from ..parsers.markdown import all_blocks, parse_markdown

WIKILINK_RE = re.compile(r"\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|[^\]]*)?\]\]")


def iter_md(root: Path):
    for p in sorted(root.rglob("*.md")):
        if ".git" in p.parts or ".index" in p.parts:
            continue
        rel = p.relative_to(root)
        if str(rel).startswith(".git"):
            continue
        yield p


def vault_paths(path: str | Path) -> tuple[Path, Path, Path]:
    vault = Path(path).resolve()
    return vault, vault / "raw", vault / ".index"


def build_index(vault: Path) -> int:
    conn = connect(vault / ".index")
    count = 0
    targets = [("raw", vault / "raw"), ("wiki", vault / "wiki")]
    seen_links: set[tuple[str, str, str]] = set()
    for layer, base in targets:
        if not base.exists():
            continue
        for md in iter_md(base):
            doc = parse_markdown(md, base)
            blocks = all_blocks(doc)
            # v0.1 has no compile layer to author §4.1 navigational summaries,
            # so derive a minimal one from the section's own content. It is a
            # *derived* projection (rebuilt every ingest), never hand-edited —
            # the compile layer will replace this with real LLM summaries in v0.3.
            for s in doc.sections:
                own = [b.raw for b in blocks if b.section_id == s.id]
                if own and not s.summary:
                    s.summary = own[0].replace("\n", " ")[:160]
            upsert_document(conn, doc)
            upsert_sections_and_blocks(conn, doc, blocks, layer=layer)
            # graph edges derived purely from Markdown (§5.2)
            for b in blocks:
                for link in WIKILINK_RE.findall(b.raw):
                    key = (b.id, link.strip(), "wikilink")
                    if key not in seen_links:
                        seen_links.add(key)
                        add_link(conn, b.id, link.strip(), "wikilink", 1.0)
            count += len(blocks)
    conn.commit()
    conn.close()
    return count


def cmd_ingest(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="koios index")
    parser.add_argument("path", nargs="?", default=".")
    parser.add_argument("--full", action="store_true")
    args = parser.parse_args(argv)
    vault = Path(args.path).resolve()
    n = build_index(vault)
    print(f"indexed {n} blocks from {vault}")
    return 0


def cmd_init(args=None) -> int:
    vault = Path(getattr(args, "path", ".")).resolve()
    for d in (
        "raw",
        "wiki/sources",
        "wiki/entities",
        "wiki/concepts",
        "wiki/synthesis",
        "wiki/answers",
        ".index",
    ):
        (vault / d).mkdir(parents=True, exist_ok=True)
    idx = vault / ".index"
    # AGENTS.md is the maintenance contract written into the vault itself (§4.4)
    agents = vault / "AGENTS.md"
    if not agents.exists():
        agents.write_text(
            "# AGENTS.md\n\n"
            "KoiosBase maintenance contract. `raw/` and `wiki/` are Markdown and are\n"
            "the source of truth; `.index/` is derived and rebuildable.\n\n"
            "- Every factual assertion in `wiki/` must carry a `[[wikilink]]` to raw.\n"
            "- New compiled pages start at `confidence: draft`.\n"
            "- Promotion requires cross-family verification or human confirmation,\n"
            "  never citation counts (§5.4).\n"
            "- Rebuild at any time: `koios index <vault>`\n",
            encoding="utf-8",
        )
    (idx / ".gitignore").write_text("*\n", encoding="utf-8")
    print(f"initialized KoiosBase vault at {vault}")
    return 0
