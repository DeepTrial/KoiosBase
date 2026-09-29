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

## Current scope (v0.8)

Shipped, per the roadmap in `docs/KoiosBase设计文档v1.3.md`:

- **ACL / multi-tenancy** (§9.2): declared in frontmatter; enforced **at
  retrieval**, so restricted text never reaches the model (masking after
  generation leaks in paraphrase)
- **MCP server** (§11): `koios mcp` speaks JSON-RPC over stdio and exposes
  `koios_search`, `koios_ask`, `koios_write_answer`. Implemented against the
  protocol directly — no SDK dependency, works offline
- **Studio exports** (§13): `koios studio brief|mindmap` — projections that add
  no new claims and carry citations (P6)
- Knowledge state machine, cascade correction, lazy synthesis, gardener (v0.4)
- Compile layer, quality gate, answer write-back, PPR weighting (v0.3)
- PDF adapter, page provenance, sources/ pages (v0.2), four channels, eval (v0.1)
- 60 pytest cases + CI (ruff, 3.10/3.11/3.12, PDF job)

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
