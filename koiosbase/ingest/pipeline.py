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
from ..parsers.pdf import parse_pdf

WIKILINK_RE = re.compile(r"\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|[^\]]*)?\]\]")


UNIVERSAL_ADAPTERS = {
    ".md": parse_markdown,
    ".markdown": parse_markdown,
    ".pdf": parse_pdf,
}


def parse_any(path: Path, root: Path):
    """Dispatch to the right adapter by extension (§5.1: new format = new adapter)."""
    adapter = UNIVERSAL_ADAPTERS.get(path.suffix.lower())
    if adapter is None:
        return None
    return adapter(path, root)


def iter_docs(root: Path):
    for p in sorted(root.rglob("*")):
        if not p.is_file() or ".git" in p.parts or ".index" in p.parts:
            continue
        if p.suffix.lower() not in UNIVERSAL_ADAPTERS:
            continue
        yield p


def vault_paths(path: str | Path) -> tuple[Path, Path, Path]:
    vault = Path(path).resolve()
    return vault, vault / "raw", vault / ".index"


def _is_derived_page(path: Path, base: Path) -> bool:
    """True for pages the tool regenerates (v0.2 sources/, v0.3 entities/).

    Kept as a single predicate so new derived dirs (concepts/, synthesis/ when
    they land) can be excluded the same way without touching the ingest loop.
    Indexing derived pages would feed the compiler's own output back in, so the
    block count would grow on every rebuild instead of staying idempotent.
    """
    try:
        rel = path.relative_to(base)
    except ValueError:
        return False
    return rel.parts[:1] in {("sources",), ("entities",)}


def build_index(vault: Path) -> int:
    conn = connect(vault / ".index")
    # Edges are DERIVED from Markdown (§5.2), so a rebuild must start from an
    # empty edge set. Previously the table was only ever appended to, so every
    # re-index duplicated every edge (5 distinct edges became 15 rows after
    # three rebuilds), which both bloated the db and inflated PPR out-weights.
    conn.execute("DELETE FROM links")
    count = 0
    raw_docs: list = []
    targets = [("raw", vault / "raw"), ("wiki", vault / "wiki")]
    seen_links: set[tuple[str, str, str]] = set()
    for layer, base in targets:
        if not base.exists():
            continue
        for md in iter_docs(base):
            # sources/ pages are DERIVED output (§4.3), not source truth (P1).
            # Indexing them would feed generated pages back into the index and
            # break idempotency — rebuilding would grow the block count forever.
            if layer == "wiki" and _is_derived_page(md, base):
                continue
            doc = parse_any(md, base)
            if doc is None:
                continue
            blocks = all_blocks(doc)
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
            if layer == "raw" and base.name == "raw":
                raw_docs.append((doc, blocks))
    # v0.2: sources/ guide pages are DERIVED output of ingest (§4.3). Written
    # only for raw-layer docs; they are Markdown truth for navigation but always
    # regenerable, so handing them to the index happens next time round.
    if raw_docs:
        from datetime import datetime, timezone

        from ..compile.sources import write_source_pages

        write_source_pages(
            vault, raw_docs, datetime.now(timezone.utc).date().isoformat()
        )
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
    # Accept either an argparse Namespace or a plain argv list (callers use both).
    path = "."
    if args is not None:
        path = getattr(args, "path", None) or (
            args[0] if isinstance(args, (list, tuple)) and args else "."
        )
    vault = Path(path).resolve()
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
