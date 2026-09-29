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
from ..retrieval.router import CJK_RE, hybrid_search


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

    blocks = filter_rows(conn, blocks, principal_groups)
    return blocks


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
    hits = 0
    for ch in set(question):
        if ch.isalnum():
            hits += 1 if ch.lower() in joined else 0
        elif "\u4e00" <= ch <= "\u9fff":
            hits += 1 if ch in joined else 0
    if hits == 0:
        return "missing"
    return "enough"


def generate(question: str, context: str, blocks: list[dict], llm=None) -> str:
    """Generate with the three contracts enforced (§6.5).

    Refusal contract: with zero evidence we return the refusal marker instead of
    letting a model improvise. Citation contract: each cited sentence repeats the
    evidence id so the chain reaches the raw block.
    """
    if not blocks:
        return f"{REFUSAL}：当前知识库中没有任何相关证据。"
    if llm is None:
        lines = []
        for b in blocks[:3]:
            snippet = b["raw"].replace("\n", " ")[:180]
            lines.append(f"- {snippet} ({b['id']})")
        return "检索到的证据如下：\n" + "\n".join(lines)
    return llm(question, context)


def _has_terms(question: str, blocks: list[dict]) -> bool:
    """True only if a REAL query term appears — never a stray single character.

    The guard intentionally ignores isolated CJK characters (年, 的, 公司…):
    they occur in virtually any Chinese corpus, so honouring them would make the
    escalation path hand the whole corpus to any question. Multi-char CJK terms
    and latin/number words count; lone characters do not.
    """
    if not blocks:
        return False
    joined = " ".join(b["raw"] for b in blocks).lower()
    runs = CJK_RE.findall(question)
    for run in runs:
        if len(run) >= 2 and run in joined:
            return True
    for tok in re.findall(r"[A-Za-z0-9]+", question):
        if tok.lower() in joined:
            return True
    return False


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
        # loophole that defeats the refusal contract (§6.5). We only escalate when
        # the full corpus actually contains evidence for THIS question; otherwise
        # we keep the empty result set so the refusal contract still fires.
        cand = _to_blocks(conn, full_corpus(conn))[: top * 5]
        if cand and _has_terms(question, cand):
            blocks = cand
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
