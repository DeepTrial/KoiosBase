"""
Minimal NLI-style entailment checker used as the L2 cross-family judge stub.

Design doc §10.3 (P7) requires that the model judging a claim differ from the
model that produced it. In v0.1 this module is a **deterministic placeholder**
that implements the *interface*; wiring a real cross-family judge (NLI
cross-encoder such as a DeBERTa-variant, or a cheap second-family API) is a v0.3
task, tracked in the roadmap. Because it is deterministic and NOT the generating
model, its verdicts are reproducible in CI.
"""

from __future__ import annotations

import re

NUM_RE = re.compile(r"\d+(?:\.\d+)?")


def judge(claim: str, evidence: str) -> str:
    """Return 'entailed' | 'contradicted' | 'unknown'.

    Placeholder rules deliberately cover only *programmatically checkable*
    relations (numbers) — everything else honestly returns 'unknown' rather than
    pretending to understand modality or negation (see §8.4: those defences
    belong to the reranker / LLM-reads-original layers, not here).
    """
    claim_nums = set(NUM_RE.findall(claim or ""))
    evid_nums = set(NUM_RE.findall(evidence or ""))
    if claim_nums and evid_nums:
        return "entailed" if claim_nums & evid_nums else "contradicted"
    return "unknown"
