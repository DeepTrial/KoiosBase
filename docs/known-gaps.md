# Known gaps

The READMEs deliberately carry no "known gaps" section — a landing page should
sell the tool, not apologise for it, and a list that falls out of date is worse
than no list. The honest accounting lives here, where it can be long, dated and
argumentative.

Every entry states what is missing, why it stays missing for now, and how to tell
it has been fixed. Verify before removing an entry.

---

## 0. Two wire formats are supported: OpenAI-shape and Anthropic

`kind = "openai"` (default) covers **every** endpoint speaking
`POST {base}/chat/completions` with Bearer auth — OpenAI, Ollama, vLLM,
llama.cpp, DeepSeek, Moonshot, Groq, Together, LiteLLM, and corporate gateways
*including those proxying Claude*, which present the OpenAI shape regardless of
the model behind them.

`kind = "anthropic"` targets Anthropic's own `/v1/messages`, which differs in
all three respects — path, auth header (`x-api-key` + `anthropic-version`, not
Bearer), and response shape (`content[0].text`, not
`choices[0].message.content`). Anything else (Google Gemini, Bedrock, ...) is
not implemented; use `--llm-cmd` or a gateway that translates for you.

Verified against a local mock for both protocols — see
`rust-cli/tests/llm_wire.rs`.

## 1. The model only writes answers

**What**: configuring a model changes nothing except the answer text. Entity
extraction, the Grader, section summaries and the L2 judge stay deterministic.

**Why**: choosing otherwise would make retrieval results depend on a network
call, and therefore non-reproducible — which would invalidate `eval`, the parity
tests and every citation already written into `wiki/`. The retrievability of a
block must be a property of the Markdown, not of today's model.

**Fixed when**: `grep -rn "llm::chat" rust-cli/src/` returns call sites outside
 `pipeline.rs`. Today it does not.

## 2. The L2 judge decides only numbers

`lint::cross_judge` extracts numerals from both strings and calls it entailed if
one matches. Anything without a number returns `unknown`.

This is honest but weak: "revenue rose" vs "revenue fell" both have no numerals
and both return `unknown`, even though they plainly contradict. Extending it
means negation/comparison detection, which either imports a model (see §1) or
hand-writes a Chinese/English negation lexicon. Neither is free, and a wrong
`contradicted` silently retracts correct pages — worse than an honest `unknown`.

**Fixed when**: `cross_judge` returns non-`unknown` for a pair of sentences that
share no numerals. Test to add: `("revenue rose", "revenue fell")` → not unknown.

## 3. ~~Stale pages are never auto-recompiled~~ — CLOSED 2026-10-25

Was: nothing called `mark_syntheses_stale` from `src/` at all, so `page_state`
stayed empty no matter how many times the vault was edited. The read side was
correct and had nothing to act on. Unit-green, system-broken. The first repair
then cleared the marker without rebuilding the body, which could present old
facts as freshly compiled.

Now closed for replayable brief and synthesis pages:

| Step | Where | Behaviour |
| --- | --- | --- |
| Flag | `cmd_index` | per-doc fingerprint diff (before/after ingest) → `syntheses_citing` → `mark_syntheses_stale` |
| Rebuild | `cmd_compile` | recover the page's raw Block ids, render the body from their current contents, then clear the flag |

Deliberate properties, each covered by `rust-cli/tests/stale_reconcile.rs`:

- **No-content-change is not a change.** Re-indexing an untouched file flags
  nothing, so a quiet vault stays quiet.
- **A brand-new vault flags nothing.** The first `index` has no fingerprint
  history to diff against; treating "was absent" as "changed" would dirty every
  export on first contact.
- **Content is rebuilt, not just re-stamped.** Brief bullets and synthesis facts
  reflect the current raw evidence. Brief creation dates, synthesis confidence,
  human conflict notes, and page lifecycle state survive recompilation.
- **Fail closed.** Missing Blocks, malformed recipes, non-generated pages, and
  unsupported templates keep their original bytes and stale flag. Missing
  evidence is never treated as an empty successful result.
- **Idempotent.** A second `compile` leaves the page byte-identical.
- **Compile writes, reads never do.** Reconciliation sits in `compile`, not in
  `search`/`query`, so a read can never mutate the vault behind your back.

**What remains**: mindmaps do not record a sufficient rebuild recipe. They stay
stale rather than being regenerated with ambiguous inputs or an implicit ACL
context. Supporting them requires persisting their root and access scope, then
reusing the section-tree builder without broadening or narrowing visibility.

## 4. The Windows binary is verified structurally only

CI crosses for `x86_64-pc-windows-gnu`, and the artefact is confirmed to be a
valid PE32+ — but no Windows runner executes it. Anything that only breaks at
runtime on Windows (path separators, console encoding, drive-letter handling)
would pass today.

Note that one real Windows-only class of bug has already been caught at compile
time: `mupdf::Document::open` takes `AsRef<FilePath>`, and `FilePath` is `str` on
Windows vs `[u8]` elsewhere, so `open_document()` is now platform-split. That is
necessary but not sufficient.

**Fixed when**: a Windows runner executes `koios init/index/lint`.

---

## Reproduction notes

`koios eval` prints metrics for *the vault you point it at*; the five seed cases
in `cmd_eval` are hardcoded to `report.md#财务分析/...` (see `main.rs:541`). No
such fixture vault ships in this repository, so there is no canonical eval number
— do not quote one as if there were. To reproduce any figure, publish the vault
alongside it.

## Closed

- **2026-10-10** — entry 3: the §8.3 write side now flags and clears stale
  synthesis pages automatically. See `rust-cli/tests/stale_reconcile.rs`.
- **2026-10-25** — entry 3: brief and synthesis reconciliation now rebuilds
  content from current raw evidence and fails closed when replay is impossible.

Last reviewed: 2026-10-25 — entry 3 updated for content recompilation; the
remaining mindmap limitation is recorded above.
