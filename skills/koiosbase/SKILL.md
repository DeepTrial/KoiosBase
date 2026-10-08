---
name: koiosbase
description: Query a KoiosBase knowledge vault and answer with citations. Use when the user asks about the contents of a KoiosBase vault, wants a cited answer from their Markdown/PDF notes, or needs to check whether a fact is actually in the vault before stating it.
---

# KoiosBase

KoiosBase is a Markdown-native knowledge base. Its defining property: **answers
carry a citation to the exact block they came from, and it refuses rather than
inventing.**

Two ways to reach it. Prefer MCP when it is connected.

## 1. MCP tools (preferred)

If the `koios` MCP server is registered you have three tools:

| Tool | Use for |
| --- | --- |
| `koios_ask` | a question → an answer whose facts carry `[[raw/...]]` citations |
| `koios_search` | the raw evidence blocks when you want to read the source yourself |
| `koios_write_answer` | write an approved answer back as a **draft** page |

Every tool takes an absolute `vault` path. There is no "current vault".

```json
{"name": "koios_ask", "arguments": {"vault": "/abs/path/to/vault", "question": "..."}}
```

## 2. CLI (fallback)

```bash
koios query "question"  -p /abs/path/to/vault     # answer + verdict
koios search "question" -p /abs/path/to/vault     # ranked blocks
koios lint /abs/path/to/vault                     # knowledge health
```

## The rule that matters

> **Never state a fact from the vault unless the answer carried a citation.**

A KoiosBase answer is one of two things, and you must tell them apart:

- **Cited** — every factual sentence ends with `[[raw/<doc>#<section>/<n>]]`.
  Safe to restate; keep the citation when you quote it.
- **Refused** — the answer is `资料中未涉及` (or `koios_ask` reports a
  `refusal` violation). The vault does not contain the answer. **Do not fill
  the gap from your own knowledge** — say it isn't there and, if useful, run
  `koios_search` to show what *is*.

This is not caution for its own sake. The whole point of the vault is that it is
auditable: an uncited claim defeats it.

## Failure handling

| Symptom | Do |
| --- | --- |
| No `koios_*` tools | run `koios mcp --install` and restart the host; meanwhile use the CLI |
| `command not found` | the binary is not on PATH; ask the user where `koios` lives and use the absolute path |
| answer has no citations | treat as not-found; do not restate it |
| `koios_ask` reports `citation=0/N` | the generator omitted citations; fall back to `koios_search` and cite the blocks yourself |
