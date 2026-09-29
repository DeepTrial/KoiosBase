"""SQLite-backed index — the .index/ layer (design doc §3.1, §5.2).

Everything here is DERIVED: `koios index` rebuilds it from raw/ + wiki/ with no
loss (P1). Four indices serve different intents:
  tree  → Section hierarchy + navigational summaries
  fts   → BM25 exact surface matching (numbers, part IDs, proper nouns)
  vec   → semantic recall (v0.1: pluggable, optional backend)
  graph → wikilinks + entity co-reference + containment + version chains
"""

import json
import sqlite3
from pathlib import Path

from ..retrieval.router import cjk_pad

SCHEMA = """
CREATE TABLE IF NOT EXISTS documents (
  path TEXT PRIMARY KEY,
  title TEXT,
  frontmatter TEXT,
  hash TEXT
);
CREATE TABLE IF NOT EXISTS sections (
  id TEXT PRIMARY KEY,
  doc_path TEXT,
  parent_id TEXT,
  title TEXT,
  summary TEXT,
  ordinal INTEGER,
  layer TEXT NOT NULL DEFAULT 'raw'
);
CREATE TABLE IF NOT EXISTS blocks (
  id TEXT PRIMARY KEY,
  doc_path TEXT,
  section_id TEXT,
  type TEXT,
  breadcrumb TEXT,
  raw TEXT,
  hash TEXT,
  page_range TEXT,
  meta TEXT,
  ordinal INTEGER NOT NULL DEFAULT 0,
  layer TEXT NOT NULL DEFAULT 'raw'
);
CREATE VIRTUAL TABLE IF NOT EXISTS blocks_fts USING fts5(
  id, breadcrumb, raw, tokenize='unicode61'
);
CREATE TABLE IF NOT EXISTS links (
  src TEXT, dst TEXT, kind TEXT, weight REAL
);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
CREATE INDEX IF NOT EXISTS idx_blocks_section ON blocks(section_id);
CREATE INDEX IF NOT EXISTS idx_links_src ON links(src);
"""


def connect(index_dir: str | Path) -> sqlite3.Connection:
    p = Path(index_dir)
    p.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(p / "tree.db"))
    conn.row_factory = sqlite3.Row
    conn.executescript(SCHEMA)
    return conn


def upsert_document(conn: sqlite3.Connection, doc) -> None:
    import json as _json

    conn.execute(
        "INSERT OR REPLACE INTO documents(path,title,frontmatter,hash) VALUES(?,?,?,?)",
        (
            doc.path,
            doc.title,
            _json.dumps(doc.frontmatter, ensure_ascii=False),
            _json.dumps([s.title for s in doc.sections]),
        ),
    )


def upsert_sections_and_blocks(
    conn: sqlite3.Connection, doc, blocks, layer: str = "raw"
) -> None:
    """layer ∈ {raw, wiki} records which truth layer a block belongs to.

    This is explicit rather than inferred from path prefixes because doc_path is
    stored relative to its own base directory (raw/ or wiki/), so a prefix test
    cannot distinguish the two layers.
    """
    for s in doc.sections:
        conn.execute(
            "INSERT OR REPLACE INTO sections(id,doc_path,parent_id,title,summary,ordinal,layer)"
            " VALUES(?,?,?,?,?,?,?)",
            (s.id, s.doc_path, s.parent_id, s.title, s.summary, s.ordinal, layer),
        )
    for b in blocks:
        conn.execute(
            "INSERT OR REPLACE INTO blocks(id,doc_path,section_id,type,breadcrumb,raw,hash,page_range,meta,ordinal,layer)"
            " VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            (
                b.id,
                b.doc_path,
                b.section_id,
                b.type,
                b.breadcrumb,
                b.raw,
                b.hash,
                json.dumps(b.page_range) if b.page_range else None,
                json.dumps(b.meta, ensure_ascii=False),
                b.ordinal,
                layer,
            ),
        )
        # keep FTS mirror in sync (standalone table, so this is a real insert)
        conn.execute("DELETE FROM blocks_fts WHERE id=?", (b.id,))
        # Store BOTH forms: the original text (so phrase terms like 营收 match)
        # and the per-character padded form (so single-char terms also match).
        # Storing only one breaks either phrases or characters — this duplication
        # keeps the BM25 index usable for both without a custom tokenizer.
        conn.execute(
            "INSERT INTO blocks_fts(id,breadcrumb,raw) VALUES(?,?,?)",
            (
                b.id,
                f"{b.breadcrumb} {cjk_pad(b.breadcrumb)}",
                f"{b.raw} {cjk_pad(b.raw)}",
            ),
        )


def add_link(
    conn: sqlite3.Connection, src: str, dst: str, kind: str, weight: float
) -> None:
    conn.execute(
        "INSERT INTO links(src,dst,kind,weight) VALUES(?,?,?,?)",
        (src, dst, kind, weight),
    )


def get_block(conn: sqlite3.Connection, block_id: str):
    row = conn.execute("SELECT * FROM blocks WHERE id=?", (block_id,)).fetchone()
    return dict(row) if row else None
