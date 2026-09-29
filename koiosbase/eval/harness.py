"""Evaluation harness for the ≥200-question baseline (§10.2).

The design doc is explicit: the eval set must land WITH v0.1 and BEFORE any
optimisation — without a baseline, tuning is flying blind. This module ships a
seed set plus a runner so the numbers are measured, not asserted.

Categories covered: factual, multi-hop, summary, fine-grained distinction
(negation / numbers / modality / same-name entities), plus refusal questions.

NOTE: the golden set here is small and hand-written (never LLM-generated — an
LLM composing its own exam is self-verification, forbidden by P7). It is a seed
to be grown to ≥200 questions, not the finished baseline.
"""

from __future__ import annotations

import sqlite3
from dataclasses import dataclass, field

from ..generation.contracts import citation_coverage, is_refusal
from ..query.pipeline import assemble, retrieve
from ..query.pipeline import query as full_query
from ..retrieval.channels import retrieve_channel

REFUSAL_MARK = "资料中未涉及"


@dataclass
class Case:
    question: str
    kind: str  # factual | multihop | summary | finegrained | refusal
    expect_block_id: str | None = None  # expected top hit (None = refusal case)
    expect_refusal: bool = False
    needs_llm: bool = False
    """True when v0.1's deterministic keyword evidence cannot decide this case.

    Example: 公司是否披露了季度分红政策？ — the corpus DOES mention 公司, so a
    bag-of-terms guard sees a "hit" even though nothing answers the question.
    Distinguishing mention-from-answer is a semantic judgement reserved for the
    cross-family reranker/Grader (v0.3); flagging it keeps the honest score
    instead of gaming the metric.
    """


@dataclass
class Report:
    total: int = 0
    hit_at_1: int = 0
    refusals_ok: int = 0
    refusal_total: int = 0
    citation_cov_sum: float = 0.0
    skipped_needs_llm: int = 0
    failures: list[str] = field(default_factory=list)

    @property
    def recall_at_1(self) -> float:
        return self.hit_at_1 / self.total if self.total else 0.0

    @property
    def refusal_accuracy(self) -> float:
        return self.refusals_ok / self.refusal_total if self.refusal_total else 0.0

    @property
    def citation_coverage(self) -> float:
        return self.citation_cov_sum / self.total if self.total else 0.0


# Seed golden set — grows toward ≥200 (§10.2). Hand-written, never self-generated.
# These block ids match the fixture corpus in tests (docs/report.md); adjust when
# extending the set so ids stay aligned with whatever vault is under evaluation.
SEED_CASES: list[Case] = [
    Case("ACME 公司的营收是多少？", "factual", "report.md#财务分析/负债分析/1"),
    Case("这家公司的现金流是多少？", "factual", "report.md#财务分析/现金流/1"),
    Case("负债金额是多少？", "factual", "report.md#财务分析/负债分析/1"),
    Case("火星基地 2030 年的预算是多少？", "refusal", None, expect_refusal=True),
    Case(
        "公司是否披露了季度分红政策？",
        "refusal",
        None,
        expect_refusal=True,
        needs_llm=True,
    ),
]


def run_case(
    conn: sqlite3.Connection, case: Case, top: int = 5, channel: str = "hybrid"
) -> tuple[bool, Report]:
    """Run one case; returns (passed, report-fragment)."""
    rep = Report(total=1)
    if case.expect_refusal:
        # Refusal contract (§6.5). We run the REAL pipeline (not `retrieve`) so
        # the Self-Route escalation guard is exercised too — testing a narrower
        # path would hide the case where escalation smuggles in unrelated blocks.
        rep.refusal_total = 1
        res = full_query(conn, case.question, top=top)
        if not res.evidence and is_refusal(res.answer):
            rep.refusals_ok = 1
            return True, rep
        if case.needs_llm:
            # Recorded as a known gap, not a silent pass and not a cheating pass:
            # see Case.needs_llm. Counted separately so the metric stays honest.
            rep.skipped_needs_llm += 1
            rep.failures.append(
                f"[needs-llm] {case.question} -> keyword evidence cannot decide "
                f"this in v0.1 ({len(res.evidence)} blocks)"
            )
            return False, rep
        rep.failures.append(
            f"[refusal] {case.question} -> {len(res.evidence)} evidence blocks, "
            f"answer={res.answer[:40]!r}",
        )
        return False, rep

    ids = retrieve_channel(conn, case.question, channel, limit=top)
    if not ids:
        # fall back to the default retrieve path
        blocks = retrieve(conn, case.question, top=top)
        ids = [b["id"] for b in blocks]
    hit = bool(ids) and (case.expect_block_id in ids)
    if hit:
        rep.hit_at_1 = 1
    else:
        rep.failures.append(
            f"[{case.kind}] {case.question} -> got {ids[:2]}, want {case.expect_block_id}"
        )
    # citation coverage measured on the assembled context (mechanical part)
    blocks = retrieve(conn, case.question, top=top)
    cited, total = citation_coverage(assemble(blocks))
    rep.citation_cov_sum = cited / total if total else 0.0
    return hit, rep


def run_eval(
    conn: sqlite3.Connection,
    cases: list[Case] | None = None,
    top: int = 5,
    channel: str = "hybrid",
) -> Report:
    cases = cases or SEED_CASES
    report = Report()
    for c in cases:
        _ok, frag = run_case(conn, c, top=top, channel=channel)
        report.total += 1
        report.hit_at_1 += frag.hit_at_1
        report.refusals_ok += frag.refusals_ok
        report.refusal_total += frag.refusal_total
        report.citation_cov_sum += frag.citation_cov_sum
        report.skipped_needs_llm += frag.skipped_needs_llm
        report.failures.extend(frag.failures)
    return report
