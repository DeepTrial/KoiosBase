"""KoiosBase — a lightweight, Markdown-native LLM knowledge base.

Three-layer model (see docs/KoiosBase设计文档v1.3.md):
  raw/    source of truth (human Markdown)
  wiki/   compiled knowledge (Markdown, agent-maintained)
  .index/ derived, fully rebuildable (SQLite + FTS5 + vectors + graph)
"""

__version__ = "0.8.2"
