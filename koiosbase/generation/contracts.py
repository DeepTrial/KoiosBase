"""Generation contracts (design doc §6.5) — enforced structurally, not by prompt.

These are the P6/§6.5 hard contracts:
  1. citation contract  — every factual sentence carries a citation to raw block
  2. refusal contract   — no evidence ⇒ say so, never pad with parametric knowledge
  3. conflict protocol  — present conflicting sources side by side, never adjudicate
"""

from __future__ import annotations

import re

from ..core.block import Block

SENTENCE_SPLIT = re.compile(r"(?<=[。！？；!?;])\s*|\n+")
CITATION_RE = re.compile(r"\[\[([^\]]+)\]\]|\(([^()]*#[^\s()]+)\)")


def citation_coverage(answer: str) -> tuple[int, int]:
    """(sentences_with_citation, total_factual_sentences) — the §10.2 metric.

    Gates a Naive-RAG-style answer that asserts without attribution: this is the
    mechanical half of the citation contract.
    """
    sentences = [s.strip() for s in SENTENCE_SPLIT.split(answer or "") if s.strip()]
    cited = sum(1 for s in sentences if CITATION_RE.search(s))
    return cited, len(sentences)


def has_citation(sentence: str) -> bool:
    return bool(CITATION_RE.search(sentence or ""))


REFUSAL = "资料中未涉及"


def is_refusal(answer: str) -> bool:
    return REFUSAL in (answer or "")


def check_refusal(evidence_blocks: list[Block], answer: str) -> bool:
    """Refusal contract violation = answering substantively with zero evidence
    and without the refusal marker."""
    if evidence_blocks:
        return True
    return is_refusal(answer)


def check_contracts(answer: str, evidence: list[Block]) -> dict:
    """Return a dict of contract violations — empty dict means all contracts met.

    Used by CI golden-set regression (§10.2), so `引用覆盖率` is measured rather
    than asserted.
    """
    cited, total = citation_coverage(answer)
    violations = {}
    if total and cited / total < 1.0:
        violations["citation"] = f"{cited}/{total} sentences cited"
    if not check_refusal(evidence, answer):
        violations["refusal"] = "answer asserted with no evidence and no refusal marker"
    return violations
