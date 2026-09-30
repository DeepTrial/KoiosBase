"""Query pipeline — v0.1 (design doc §6).

All FOUR v0.1 channels are wired here:
  ② hybrid recall (BM25 + optional vector, RRF-fused)
  ③ PPR graph expansion
  ① tree navigation (LLM-free by default; optional cross-family judge)
  ④ full-corpus mode — also the Grader's escalation path (Self-Route, §6.2)

P3 — retrieval is a decision loop, not a top-k dump: the Grader inspects the
evidence and may escalate (retrieval → full corpus) rather than answering blind.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

from ..core.block import Block
from ..generation.contracts import REFUSAL, check_contracts
from ..index.schema import get_block
from ..retrieval.channels import (
    full_corpus,
    retrieve_channel,
    tree_search,
)
from ..retrieval.router import hybrid_search
from ..security.acl import filter_rows
from ..state.model import apply_disposition, filter_visible


@dataclass
class QueryResult:
    answer: str = ""
    evidence: list[dict] = field(default_factory=list)
    violations: dict = field(default_factory=dict)
    trace: dict = field(default_factory=dict)


def _to_blocks(conn, ids: list[str]) -> list[dict]:
    out = []
    for bid in ids:
        raw = get_block(conn, bid)
        if raw:
            out.append(raw)
    return out


def retrieve(
    conn,
    question: str,
    top: int = 8,
    use_graph: bool = True,
    channel: str | None = None,
    principal_groups: set[str] | None = None,
) -> list[dict]:
    """Retrieve evidence for a question.

    `channel` selects one route: hybrid | tree | full | graph. The default path
    is ② hybrid with the ③ graph bonus, which is what most queries need.
    """
    if channel == "full":
        ids = full_corpus(conn)[: top * 20]
    elif channel == "tree":
        ids = tree_search(conn, question)[:top]
    elif channel:
        ids = retrieve_channel(conn, question, channel, limit=top)
    else:
        rows = hybrid_search(conn, question, limit=top, use_graph=use_graph)
        ids = [d for d, _s in rows]
    blocks = _to_blocks(conn, ids)
    # §9.2: ACL is enforced HERE, before context assembly — filtering after
    # generation would leak in paraphrase, because a block that reached the model
    # has already shaped the answer. `principal_groups=None` means ANONYMOUS and
    # must still be filtered: treating None as "skip the check" is what leaked
    # restricted documents to unauthenticated callers.
    from ..security.acl import filter_rows
    from ..state.model import filter_visible

    blocks = filter_rows(conn, blocks, principal_groups)
    # §8.2 hard filter: a retracted/superseded block must never enter the
    # context, on ANY path. The state machinery was written and unit-tested but
    # never reached the read path, so `koios retract` had no observable effect
    # on search / ask / MCP / eval — the cascade marked pages stale while the
    # offending block itself kept answering questions.
    blocks = filter_visible(blocks, conn)
    # §8.3 query-time disposition: stale pages are down-ranked (never silently
    # treated as fresh) and a retracted page is dropped entirely.
    blocks = apply_disposition(conn, blocks)
    return blocks


def score_blocks(conn, question: str, blocks: list[dict]) -> list[tuple[float, dict]]:
    """Rank already-loaded blocks by BM25 relevance — no second storage pass.

    Used by the Self-Route escalation, which needs `full_corpus` order (prompt-
    cache stability) replaced by relevance order. Scoring is done through the
    FTS index so the ranking path has exactly one definition of relevance.
    """
    from ..retrieval.router import fts_search

    order: dict[str, float] = {}
    ranked = fts_search(conn, question, limit=max(20, len(blocks)))
    for rank, (bid, score) in enumerate(ranked):
        order[bid] = -rank if score is None else score
    floor = float("-inf")
    return sorted(
        ((order.get(b["id"], floor), b) for b in blocks),
        key=lambda pair: -pair[0],
    )


def assemble(blocks: list[dict], budget_chars: int = 6000) -> str:
    """Context assembly with budget discipline (§6.4) — hard cap, compress
    rather than keep stuffing."""
    parts = []
    total = 0
    for b in blocks:
        piece = f"[{b['id']}] ({b['breadcrumb']})\n{b['raw']}\n"
        if total + len(piece) > budget_chars:
            remain = budget_chars - total
            if remain > 120:
                parts.append(piece[:remain] + " …")
            break
        parts.append(piece)
        total += len(piece)
    return "\n---\n".join(parts)


def _shared_terms(question: str, joined: str) -> list[str]:
    """The query terms that actually occur in `joined` — one definition of a hit.

    `grade()` (main verdict) and `_has_terms()` (escalation guard) used different
    strictness: grade counted any single character, including 年/的, so it said
    `enough` on noise; the guard demanded a ≥2-char CJK run or a latin token, so
    it said "no real term" on the same evidence. The asymmetry let weak evidence
    pass the Grader while a stricter bar governed escalation — the loophole that
    makes Self-Route either too eager or dead. Both now read this one function.
    """
    found: list[str] = []
    # A lone CJK character occurs in virtually any Chinese corpus, so honouring
    # one (年, 的) would let any question match any document — this is why both
    # the verdict and the escalation guard require a multi-character run.
    for run in re.findall(r"[\u4e00-\u9fff]+", question):
        if len(run) < 2:
            continue  # a lone character occurs in nearly any Chinese corpus
        if run in joined:
            found.append(run)
    for tok in re.findall(r"[A-Za-z0-9]+", question):
        if tok.lower() in joined:
            found.append(tok.lower())
    return found


def grade(question: str, blocks: list[dict]) -> str:
    """Grader verdict (§6.2): enough | missing | conflict | evidence_absent.

    Deterministic in v0.1 — the cross-family LLM Grader (P7) arrives with the
    compile layer, but the escalation CONTRACT is already enforced here. Note
    `evidence_absent` is reserved for genuinely zero evidence; a non-empty but
    weakly-matching set is `missing`, which lets the caller retry another channel
    without silently pretending the corpus has nothing.
    """
    if not blocks:
        return "evidence_absent"
    joined = " ".join(b["raw"] for b in blocks)[:2000].lower()
    if not _shared_terms(question, joined):
        return "missing"
    return "enough"


def _cited(snippet: str, block_id: str) -> str:
    return f"- {snippet} [[raw/{block_id}]]"


def generate(question: str, context: str, blocks: list[dict], llm=None) -> str:
    """Generate with the three contracts enforced (§6.5).

    Refusal contract: with zero evidence we return the refusal marker instead of
    letting a model improvise. Citation contract: each cited sentence repeats the
    evidence id so the chain reaches the raw block.
    """
    if not blocks:
        return f"{REFUSAL}：当前知识库中没有任何相关证据。"
    if llm is None:
        # The built-in generator must pass `check_contracts` like any other
        # producer — a header line with no citation made every default answer a
        # citation violation, which quietly trained callers to ignore that signal.
        lines = []
        for b in blocks[:3]:
            snippet = b["raw"].replace("\n", " ")[:180]
            lines.append(_cited(snippet, b["id"]))
        return "\n".join(lines)
    return llm(question, context)


def _has_terms(question: str, blocks: list[dict]) -> bool:
    """True only if a REAL query term appears — never a stray single character.

    Shares `_shared_terms` with `grade()` so the two cannot drift apart again
    (see that function for why the previous strictness mismatch mattered).
    """
    if not blocks:
        return False
    joined = " ".join(b["raw"] for b in blocks).lower()
    return bool(_shared_terms(question, joined))


def query(
    conn,
    question: str,
    top: int = 8,
    llm=None,
    use_graph: bool = True,
    allow_full: bool = True,
    channel: str | None = None,
    principal_groups: set[str] | None = None,
) -> QueryResult:
    """Full pipeline with Grader-driven escalation (Self-Route, §6.2).

    Retrieval first; if the Grader says the evidence is absent AND the corpus is
    small enough, escalate to channel ④ rather than fabricating.
    """
    blocks = retrieve(
        conn,
        question,
        top=top,
        use_graph=use_graph,
        channel=channel,
        principal_groups=principal_groups,
    )
    verdict = grade(question, blocks)
    trace: dict = {
        "question": question,
        "hits": len(blocks),
        "channel": channel or "hybrid+ppr",
        "verdict": verdict,
    }
    if verdict == "evidence_absent" and allow_full:
        # This branch does NOT have a loophole that defeats the refusal contract
        # (§6.5). We only escalate when the full corpus actually contains evidence
        # for THIS question; otherwise we keep the empty result set so the refusal
        # contract still fires.
        cand = _to_blocks(conn, full_corpus(conn))
        cand = filter_rows(conn, cand, principal_groups)
        cand = filter_visible(cand, conn)
        if cand and _has_terms(question, cand):
            # Re-rank instead of taking `full_corpus` order: that order is
            # doc_path/ordinal — chosen for prompt-cache stability, not relevance.
            # Substituting it wholesale meant the escalation path answered with
            # the first blocks in the library rather than the best ones, and threw
            # away every hybrid score that had already ranked these blocks.
            scored = score_blocks(conn, question, cand)
            blocks = [b for _s, b in scored][: top * 5]
            trace["escalated"] = "full"
            trace["verdict"] = grade(question, blocks)
    context = assemble(blocks)
    answer = generate(question, context, blocks, llm=llm)
    violations = check_contracts(
        answer,
        [
            Block(
                id=b["id"],
                type=b["type"],
                breadcrumb=b["breadcrumb"],
                raw=b["raw"],
                doc_path=b["doc_path"],
            )
            for b in blocks
        ],
    )
    return QueryResult(
        answer=answer, evidence=blocks, violations=violations, trace=trace
    )
