"""Quality gate for compiled pages (design doc §5.4) and answer write-back (§6.6).

Two rules carry most of the weight here:

1. **Promotion never uses citation counts.** Counting citations is the obvious
   implementation and it is wrong: it is the same signal PPR uses, so a draft page
   could bootstrap itself into authority (§6.6 / §14 回流污染循环). Promotion goes
   through cross-family verification or human confirmation only.

2. **New pages start at `confidence: draft`.** draft pages are down-weighted in
   retrieval but not thrown away (§5.4: 不浪费，不轻信).
"""

from __future__ import annotations

import re
from pathlib import Path

CONFIDENCE_RE = re.compile(r"^confidence:\s*(\w+)", re.MULTILINE)
VALID = {"draft", "medium", "high"}
# §6.6: out-edge weight by source page confidence — unverified pages confer NO
# authority, which is what cuts the "citations -> authority -> more citations" loop.
CONFIDENCE_WEIGHT = {"draft": 0.0, "medium": 0.5, "high": 1.0}


def read_confidence(path: str | Path) -> str:
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError:
        return "draft"
    m = CONFIDENCE_RE.search(text)
    return m.group(1) if m and m.group(1) in VALID else "draft"


def can_promote(
    confidence: str, *, verified: bool = False, human_confirmed: bool = False
) -> bool:
    """Promote only via cross-family verification or human confirmation (§5.4)."""
    if confidence not in VALID or confidence == "high":
        return False
    return bool(verified or human_confirmed)


def promote(
    path: str | Path, *, verified: bool = False, human_confirmed: bool = False
) -> str:
    """Raise one step: draft -> medium -> high. Returns the new confidence."""
    p = Path(path)
    try:
        text = p.read_text(encoding="utf-8")
    except OSError:
        return "draft"
    cur = read_confidence(p)
    if not can_promote(cur, verified=verified, human_confirmed=human_confirmed):
        return cur
    nxt = "medium" if cur == "draft" else "high"
    if CONFIDENCE_RE.search(text):
        text = CONFIDENCE_RE.sub(f"confidence: {nxt}", text, count=1)
    else:
        text = text.replace("---\n", f"---\nconfidence: {nxt}\n", 1)
    p.write_text(text, encoding="utf-8")
    return nxt


def edge_weight(confidence: str) -> float:
    """PPR out-edge weight for a page of this confidence (§6.6)."""
    return CONFIDENCE_WEIGHT.get(confidence, 0.0)


ANSWER_TEMPLATE = """---
type: answer
confidence: draft
generated: true
created: {stamp}
sources: [{sources}]
---

# {question}

{answer}

## 依据
{evidence}
"""


def write_answer_page(
    vault, question: str, answer: str, evidence: list[dict], stamp: str
) -> Path:
    """Answer write-back (§6.6): the approved answer becomes a draft page."""
    out_dir = vault / "wiki" / "answers"
    out_dir.mkdir(parents=True, exist_ok=True)
    slug = (
        re.sub(r"[^\w\u4e00-\u9fff]+", "-", question.strip())[:60].strip("-")
        or "answer"
    )
    target = out_dir / f"{slug}.md"
    ev = "\n".join(f"- [[raw/{b['id']}]]" for b in evidence[:8]) or "- (无)"
    target.write_text(
        ANSWER_TEMPLATE.format(
            stamp=stamp,
            question=question,
            answer=answer,
            sources=", ".join(f"raw/{b['id']}" for b in evidence[:3]),
            evidence=ev,
        ),
        encoding="utf-8",
    )
    return target
