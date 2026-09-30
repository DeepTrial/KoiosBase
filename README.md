[![CI](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepTrial/KoiosBase/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/DeepTrial/KoiosBase)](https://github.com/DeepTrial/KoiosBase/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[English](README.md) | [中文](README.zh.md) | [日本語](README.ja.md)

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
| Build artifacts | `.index/` — BM25, graph; always rebuildable |
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

Prebuilt binaries (no Python needed) are attached to each
[release](https://github.com/DeepTrial/KoiosBase/releases):
`koios-linux-x86_64` (static musl) and `koios-windows-x86_64.exe`.

## Quick start

```bash
koios init myvault          # scaffold a vault + AGENTS.md contract
# edit myvault/raw/*.md
koios index myvault         # build the derived index (idempotent)
koios search "营收" -p myvault
koios lint myvault          # L1 programmatic health checks
koios checkclaim "营收 32 亿元" "营收 32 亿元"
```

## Two shells, one vault

KoiosBase ships a Python implementation and a Rust one. Both read and write the
**same** vault format — `raw/` + `wiki/` are the truth, `.index/` is derived, so
you can index with either and query with either.

The Python CLI is the reference implementation and covers every command. The
Rust binary covers the same command surface and is verified against Python
output on a shared fixture (block ids, breadcrumbs, generated pages, MCP
responses compared byte-for-byte).

```bash
koios search "营收" -p myvault -c tree     # force a channel (§6.1)
koios search "营收" -p myvault --groups finance-team   # ACL principal (§9.2)
koios eval -p myvault                      # eval baseline
koios eval -p myvault --strict             # also fail on known semantic gaps
```

## Current scope (v0.8.1)

- **ACL / multi-tenancy** (§9.2): declared in frontmatter; enforced **at
  retrieval**, so restricted text never reaches the model. Derived `sources/`
  pages inherit the grant of the raw document they were compiled from — a
  generated page declares no `acl` of its own, so reading it literally would
  pronounce restricted content public.
- **Knowledge state machine** (§8.2/§8.3): blocks carry `active | superseded |
  disputed | retracted | draft`; retracting propagates along citation links.
  Retracted blocks are hard-filtered from every read path. Answers citing a
  page whose sources moved on are marked （待更新）.
- **MCP server** (§11): `koios mcp` speaks JSON-RPC over stdio and exposes
  `koios_search`, `koios_ask`, `koios_write_answer`. Implemented against the
  protocol directly — no SDK dependency, works offline.
- **Studio exports** (§13): `koios studio brief|mindmap` — projections that add
  no new claims and carry citations (P6). Both filtered by ACL: an export is
  written to disk, so a leak there outlives the request.
- **Compile layer** (§5.3/§5.4): entity and source pages; promotion requires
  cross-family verification or human confirmation, never citation counts.
  Recompiling preserves a page's promoted confidence and human-authored notes.
- **PDF adapter** (§5.1): CPU text tier with page-level provenance, so
  citations point at an exact page. The VLM tier is a hook, not a dependency.
- **Four channels** (§6.1) + Grader-driven Self-Route escalation (§6.2).
- 65 pytest cases + CI (ruff, Python 3.10/3.11/3.12, PDF job).

### Known gaps, tracked rather than hidden

These are documented limits, not oversights. Please read them before relying on
the system.

- **`needs_llm` cases.** `koios eval` reports a `needs_llm` count: questions
  where keyword evidence sees a hit but no real answer exists (mentioning 公司
  ≠ answering a dividend-policy question). Distinguishing mention from answer
  needs the cross-family reranker/Grader of v0.3, so they are counted as gaps
  instead of silently passed.
- **The L2 judge is a deterministic placeholder.** It implements the interface
  and decides only programmatically checkable relations (numbers); everything
  else honestly returns `unknown`.
- **Compile write set is O(corpus).** Entity pages are recompiled wholesale;
  correctness (preserving confidence and curated sections) is solved, but the
  affected set is computed and then not used to narrow the writes. Tracked as
  tech debt; see `docs/KoiosBase设计文档v1.3.md`.
- **Async recompile on stale is unimplemented.** A stale page is disclosed and
  down-ranked, but the recompile that would clear it is not triggered
  automatically (§5.3 forbids blocking availability on it).
- **The Windows binary is verified structurally only.** It builds and is a valid
  PE32+ executable, but no Windows runner or Wine was available to execute it.

## Layout

```
koiosbase/
  core/        Block / Section / Document data model
  parsers/     format adapters (Markdown, PDF)
  ingest/      raw + wiki -> derived index
  index/       SQLite schema (tree, blocks, FTS, links)
  retrieval/   BM25 search, RRF fusion, personalized PageRank
  generation/  citation + refusal contracts, cross-family judge
  compile/     entity + source pages, quality gate
  state/       knowledge state machine and cascade
  lint/        L1 programmatic gardener
  query/       retrieve -> assemble -> generate pipeline
  security/    ACL / multi-tenancy
  mcp/         JSON-RPC server over stdio
  studio/      brief / mindmap exports
  cli.py       koios command-line interface
rust-cli/      the Rust shell (same vault format)
```

## Docs

- `docs/KoiosBase设计文档v1.3.md` — the full design baseline (Chinese)
- `AGENTS.md` — the maintenance contract written into each vault (§4.4)

## License

MIT — see [LICENSE](LICENSE).
