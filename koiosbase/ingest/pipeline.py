"""Ingest pipeline: raw/ + wiki/ → .index/ (design doc §5).

Synchronous indexing only in v0.1; the compile stage (entities/concepts) lands
in v0.3 (§13). Ingestion is **idempotent, not incremental**: every document is
re-parsed and its rows upserted on each run, so a rebuild reproduces exactly the
same block set from the same Markdown. Content hashes make that upsert cheap and
stable — they do not skip unchanged documents, and no doc is left unvisited.
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

    Every derived directory is handled the SAME way: all of them are indexed. Excluding some (sources/, entities/) while letting others in
    (synthesis/, answers/) — the old behaviour — split one worry two ways for no
    reason: content hashes make ingestion idempotent, so a derived page cannot
    grow the block count across rebuilds whatever it contains. Self-feeding is
    prevented where it actually belongs, on the compile side, whose source set is
    pinned to `layer='raw'` (see compile/pipeline.py).

    Keeping entities/ OUT of the index also silently starved channel ⓪
    (§6.1, wiki-first retrieval): there were no wiki rows for it to prefer.

    Kept as a predicate so the set of derived dirs stays inspectable in one
    place; it now drives layer tagging and bookkeeping rather than exclusion.
    """
    try:
        rel = path.relative_to(base)
    except ValueError:
        return False
    return rel.parts[:1] in {
        ("sources",),
        ("entities",),
        ("synthesis",),
        ("answers",),
    }


DERIVED_LAYERS = ("sources", "entities", "synthesis", "answers")

# Escape hatch for vaults where re-indexing derived pages would be disruptive
# during a migration. Default OFF: derived pages belong in the index (see above).
EXCLUDE_DERIVED = False


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
    # Derived pages are generated BEFORE the wiki walk so the same rebuild that
    # creates them also indexes them — otherwise the first pass counts 3 blocks,
    # the second counts 7, and `build_index` looks non-idempotent forever.
    _write_source_pages(vault, _read_raw_docs(vault))
    for layer, base in targets:
        if not base.exists():
            continue
        for md in iter_docs(base):
            # Derived pages (sources/ entities/ synthesis/ answers/) ARE
            # indexed — see `_is_derived_page`. Content hashes make ingestion
            # idempotent regardless of who wrote the file, so counting them in
            # cannot grow the block set across rebuilds; excluding them starved
            # channel ⓪ (§6.1). Self-feeding is stopped on the compile side,
            # whose inputs are pinned to `layer='raw'`.
            if layer == "wiki" and EXCLUDE_DERIVED and _is_derived_page(md, base):
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
    # v0.2: sources/ guide pages are DERIVED output of ingest (§4.3) — written
    # at the head of the run (see above) so the wiki pass below indexes them.
    conn.commit()
    conn.close()
    return count


def _read_raw_docs(vault: Path) -> list:
    """Parse every raw-layer doc through the adapter map (§5.1).

    raw/ can hold PDFs, and parsing those as Markdown crashes with
    UnicodeDecodeError, so the adapter must be chosen by suffix.
    """
    docs = []
    base = vault / "raw"
    if not base.exists():
        return docs
    for p in sorted(iter_docs(base)):
        doc = parse_any(p, base)
        if doc is None:
            continue
        docs.append((doc, all_blocks(doc)))
    return docs


def _write_source_pages(vault: Path, raw_docs: list) -> None:
    """Regenerate sources/ navigation pages from the raw corpus (§4.3)."""
    if not raw_docs:
        return
    from datetime import datetime, timezone

    from ..compile.sources import write_source_pages

    write_source_pages(vault, raw_docs, datetime.now(timezone.utc).date().isoformat())


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
