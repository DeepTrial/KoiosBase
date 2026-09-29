"""Query pipeline — v0.1 (design doc §6).

Ships channel ② hybrid recall (vector + BM25 fused via RRF) with the ③ PPR
graph bonus, plus the generate-stage contracts. Channels ⓪ (wiki-first) and
① (tree navigation) arrive with the compile layer in v0.3; ④ (full-corpus)
is the Grader escalation path (§6.2 Self-Route).

P3 — retrieval is a decision loop, not a top-k dump. Even in v0.1 the Grader
runs: it decides whether evidence is enough instead of blindly answering.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from ..core.block import Block
from ..generation.contracts import REFUSAL, check_contracts
from ..index.schema import get_block
from ..retrieval.router import hybrid_search


@dataclass
class QueryResult:
    answer: str = ""
    evidence: list[dict] = field(default_factory=list)
    violations: dict = field(default_factory=dict)
    trace: dict = field(default_factory=dict)


def retrieve(conn, question: str, top: int = 8, use_graph: bool = True):
    rows = hybrid_search(conn, question, limit=top, use_graph=use_graph)
    blocks = []
    for bid, score in rows:
        raw = get_block(conn, bid)
        if raw:
            raw["_score"] = score
            blocks.append(raw)
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


def generate(question: str, context: str, blocks: list[dict], llm=None) -> str:
    """Generate with the three contracts enforced (§6.5).

    Refusal contract: with zero evidence we return the refusal marker instead of
    letting a model improvise. Citation contract: each cited sentence repeats the
    evidence id so the chain reaches the raw block.
    """
    if not blocks:
        return f"{REFUSAL}：当前知识库中没有任何相关证据。"
    if llm is None:
        # Deterministic extractive fallback — used by CI so the pipeline is
        # testable without any model dependency.
        lines = []
        for b in blocks[:3]:
            snippet = b["raw"].replace("\n", " ")[:180]
            lines.append(f"- {snippet} ({b['id']})")
        return "检索到的证据如下：\n" + "\n".join(lines)
    return llm(question, context)


def query(
    conn, question: str, top: int = 8, llm=None, use_graph: bool = True
) -> QueryResult:
    blocks = retrieve(conn, question, top=top, use_graph=use_graph)
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
        answer=answer,
        evidence=blocks,
        violations=violations,
        trace={"question": question, "hits": len(blocks), "channel": "hybrid+ppr"},
    )
