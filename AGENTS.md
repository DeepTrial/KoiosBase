# AGENTS.md

KoiosBase maintenance contract. `raw/` and `wiki/` are Markdown and are
the source of truth; `.index/` is derived and rebuildable.

- Every factual assertion in `wiki/` must carry a `[[wikilink]]` to raw.
- New compiled pages start at `confidence: draft`.
- Promotion requires cross-family verification or human confirmation,
  never citation counts (§5.4).
- Rebuild at any time: `koios index <vault>`
