"""Compile stage entry point (design doc §5.3).

Runs AFTER indexing, asynchronously in spirit (here: a separate CLI step so a
slow/failed compile never blocks query availability). Affected pages are
computed from the index, never by scanning the library.
"""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path

from ..index.schema import connect
from ..parsers.markdown import all_blocks
from .entities import (
    affected_concepts,
    affected_entities,
    extract_entities,
    write_entity_pages,
)
from .sources import write_source_pages


def compile_vault(vault: str | Path) -> dict:
    """Compile entities/ from raw docs; returns a summary dict."""
    vault = Path(vault)
    stamp = datetime.now(timezone.utc).date().isoformat()
    conn = connect(vault / ".index")

    rows = conn.execute(
        "SELECT DISTINCT doc_path FROM blocks WHERE layer='raw'"
    ).fetchall()
    # Route through the adapter map (§5.1): raw/ can hold PDFs, and parsing those
    # as Markdown crashes with UnicodeDecodeError.
    from ..ingest.pipeline import UNIVERSAL_ADAPTERS, parse_any

    raw_docs = []
    for r in rows:
        p = vault / "raw" / r["doc_path"]
        if not p.exists() or p.suffix.lower() not in UNIVERSAL_ADAPTERS:
            continue
        doc = parse_any(p, vault / "raw")
        if doc is None:
            continue
        raw_docs.append((doc, all_blocks(doc)))

    entities = {}
    touched_pages = 0
    for doc, blocks in raw_docs:
        if not blocks:
            continue
        found = extract_entities(blocks)
        # §5.3 ② affected-set via index lookups, not a library scan
        touched_pages += len(affected_entities(conn, found))
        touched_pages += len(affected_concepts(conn, blocks))
        for eid, ent in found.items():
            if eid in entities:
                entities[eid].facts.extend(ent.facts)
            else:
                entities[eid] = ent

    n_pages = write_entity_pages(vault, entities, stamp)
    n_sources = write_source_pages(vault, raw_docs, stamp)
    conn.close()
    return {
        "entities": len(entities),
        "entity_pages": n_pages,
        "source_pages": n_sources,
        "affected_pages": touched_pages,
        "stamp": stamp,
    }
