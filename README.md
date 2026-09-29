[![CI](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/DeepTrial/KoiosBase)](https://github.com/DeepTrial/KoiosBase/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[中文版](README.zh.md) | English version

# KoiosBase

KoiosBase is a lightweight, Markdown-native LLM knowledge base.

The dominant RAG pattern — *slice documents into chunks, embed, retrieve top-k by
similarity* — throws away structure, relations and lifecycle. Every fix layered
on top (hybrid search, reranking, chunk contextualization) treats symptoms.

KoiosBase takes a different stance: **treat a knowledge base as software
engineering that is continuously built**.

| Software engineering | KoiosBase |
| --- | --- |
| Source repository | `raw/` — human-maintained Markdown (the vault) |
| Compiler | compile layer — synthesizes sources into structured pages at ingest |
| Build artifacts | `.index/` — vectors, BM25, graph; always rebuildable |
| Runtime | query pipeline — navigate and reason over artifacts |
| Lint / CI | the gardener — periodic health checks |
| Version control | Git + version chains |

## Why it differs from plain RAG

| | Plain RAG | KoiosBase |
| --- | --- | --- |
| Storage | flat chunk pool | three-layer model (raw / compiled / derived) |
| Retrieval | one-shot top-k | channel routing + sufficiency iteration |
| Growth | append-only | ingest compilation + answer feedback + lint self-healing |
| Correction | re-chunk and re-embed | state flags + propagation along citation links |
| Trust | similarity score | citation contract + page-level provenance |

## First principles

Every mechanism derives from seven principles; no patch-work components allowed:

- **P1** Markdown is the single source of truth.
- **P2** Store the finest granularity; retrieve at any granularity (views).
- **P3** Retrieval is navigation and reasoning, never a similarity contest.
- **P4** Knowledge is synthesized at ingest, not recomputed per query.
- **P5** Knowledge has state; errors propagate and are repairable.
- **P6** Every assertion is auditable back to its source.
- **P7** The judge must differ in origin from the generator (anti-self-verification).

## Install

Requires Python 3.10+ (SQLite with FTS5 — bundled in CPython).

```bash
pip install -e ".[test]"
```

## Quick start

```bash
koios init myvault          # scaffold a vault + AGENTS.md contract
# edit myvault/raw/*.md
koios index myvault         # build the derived index (idempotent)
koios search "营收" -p myvault
koios lint myvault          # L1 programmatic health checks
koios checkclaim "营收 32 亿元" "营收 32 亿元"
```

## Current scope (v0.2)

Shipped, per the roadmap in `docs/KoiosBase设计文档v1.3.md`:

- **PDF adapter with double-tier fallback** (§5.1): CPU tier via PyMuPDF by
  default, VLM precision tier when the page looks scanned (escalation is a
  callable, so v0.2 ships with no model dependency)
- **Page-level provenance**: every PDF Block records `page_range`, so citations
  can be page-exact (`§6.5`)
- **`sources/` guide pages** (§4.3): one derived page per raw document with a
  reading summary and the questions it answers; regenerated on every ingest and
  excluded from indexing so rebuilds stay idempotent (P1)
- Markdown parser → document tree → atomic Blocks (tables/code/lists never split)
- Breadcrumb injection for dangling-reference resolution (zero LLM cost)
- Derived index: sections/blocks in SQLite, BM25 via FTS5, wikilink graph
- **Four retrieval channels** (§6.1): ① tree navigation, ② hybrid BM25+vector
  with RRF fusion, ③ PPR graph expansion, ④ full-corpus with Self-Route escalation
- Generation contracts: citation coverage, refusal, no-evidence detection
- L1 gardener: broken links + unsourced assertions
- **Eval harness** (§10.2) reporting recall@1, refusal accuracy, citation coverage
- 31 pytest cases + GitHub Actions CI (ruff, tests on 3.10/3.11/3.12, PDF job)

```bash
koios search "营收" -p myvault -c tree     # force channel ①
koios eval -p myvault                      # eval baseline (needs-llm cases skipped)
koios eval -p myvault --strict             # fail on known semantic gaps too
```

Known limitation honestly tracked, not hidden: `koios eval` reports `needs_llm`
count. Those are questions where keyword evidence sees a hit but no real answer
(e.g. mentioning 公司 ≠ answering a dividend-policy question). Distinguishing
mention from answer needs the cross-family reranker/Grader arriving in v0.3, so
they are counted as gaps rather than silently passed.

Not yet implemented (planned in later versions): the compile layer
(entities/concepts) including channel ⓪ wiki-first and LLM-authored
navigational summaries, PDF adapters, the VLM sampling loop, and the full
cross-family L2 judge — the current judge is a deterministic placeholder
implementing the interface only.

## Layout

```
koiosbase/
  core/        Block / Section / Document data model
  parsers/     format adapters (Markdown today)
  ingest/      raw + wiki -> derived index
  index/       SQLite schema (tree, blocks, FTS, links)
  retrieval/   BM25 search, RRF fusion, personalized PageRank
  generation/  citation + refusal contracts, cross-family judge
  lint/        L1 programmatic gardener
  query/       retrieve -> assemble -> generate pipeline
  cli.py       koios command-line interface
```

## Docs

- `docs/KoiosBase设计文档v1.3.md` — the full design baseline (Chinese)

## License

MIT — see [LICENSE](LICENSE).
