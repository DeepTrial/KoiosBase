"""Channel ① tree-search navigation and channel ④ full-corpus mode (§6.1).

① tree navigation — instead of scoring every block, walk the Section tree and
   ask at each level "is this subtree worth expanding?" Sections carry
   *navigational* summaries (what questions it answers / which entities it
   covers), which is exactly the signal the drill-down decision needs.
② full-corpus mode  — when the corpus is small enough (< threshold tokens) the
   whole tree is injected instead of retrieved; this is ALSO the Grader's
   escalation path when it returns `evidence_absent` (Self-Route, §6.2).

Both are deterministic and LLM-free by default: the drill-down scoring uses BM25
over section summaries, and only escalates to an `llm_judge` callback when one is
supplied. Keeping the default model-free means v0.1 works offline and stays
testable in CI.
"""

from __future__ import annotations

import re
import sqlite3

from .router import cjk_pad, query_terms, rrf_fuse

TOKEN_RE = re.compile(r"[\w\u4e00-\u9fff]+")


def _terms(text: str) -> set[str]:
    return {t.lower() for t in TOKEN_RE.findall(text or "")}


def score_section(summary: str, title: str, qterms: set[str]) -> float:
    """Navigational relevance of a section (title + navigational summary).

    Both sides are CJK-padded so the comparison is per-character, matching how
    the §6.1 drill-down decision actually behaves on Chinese text.
    """
    if not qterms:
        return 0.0
    padded_q = {t for t in cjk_pad(" ".join(qterms)).split()}
    hay = {t for t in cjk_pad(title + " " + (summary or "")).split()}
    if not hay or not padded_q:
        return 0.0
    hits = len(hay & padded_q)
    return hits / len(padded_q)


def tree_search(
    conn: sqlite3.Connection, query: str, top_sections: int = 3, llm_judge=None
) -> list[str]:
    """Channel ①: return block ids from the most promising sections.

    Deterministic ranking over the section tree; `llm_judge(title, summary) -> float`
    may override the score when a cross-family judge is available (P7).
    """
    qterms = {t.lower() for t in query_terms(query)}
    if not qterms:
        return []
    rows = conn.execute("SELECT id,title,summary FROM sections").fetchall()
    scored = []
    for r in rows:
        if not (r["title"] or r["summary"]):
            continue
        score = score_section(r["summary"], r["title"], qterms)
        if score <= 0:
            continue
        scored.append((r["id"], score, r["title"], r["summary"]))
    if not scored:
        return []
    if llm_judge is not None:
        scored = [(i, llm_judge(t, s) or sc, t, s) for i, sc, t, s in scored]
    scored.sort(key=lambda x: -x[1])
    picked = scored[:top_sections]
    ids: list[str] = []
    for sid, _sc, _t, _s in picked:
        blocks = conn.execute(
            "SELECT id FROM blocks WHERE section_id=? ORDER BY ordinal", (sid,)
        ).fetchall()
        ids.extend(b["id"] for b in blocks)
    return ids


def corpus_size(conn: sqlite3.Connection) -> int:
    """Total characters across raw + wiki blocks — the ④ threshold input."""
    row = conn.execute(
        "SELECT COALESCE(SUM(LENGTH(raw)),0) AS n FROM blocks"
    ).fetchone()
    return int(row["n"] or 0)


def full_corpus(conn: sqlite3.Connection, max_chars: int = 200_000) -> list[str]:
    """Channel ④: every block, ordered for prompt-cache friendliness.

    Returns [] when the corpus exceeds the threshold — the caller must NOT then
    pretend a full read happened; it should fall back to retrieval (§6.4 budget
    discipline).
    """
    size = corpus_size(conn)
    if size > max_chars:
        return []
    rows = conn.execute("SELECT id FROM blocks ORDER BY doc_path, ordinal").fetchall()
    return [r["id"] for r in rows]


def retrieve_channel(
    conn: sqlite3.Connection,
    query: str,
    channel: str,
    limit: int = 8,
    max_chars: int = 200_000,
    **kw,
) -> list[str]:
    """Dispatch a named channel. Returns block ids."""
    from .router import hybrid_search

    if channel == "tree":
        return tree_search(conn, query, llm_judge=kw.get("llm_judge"))
    if channel == "full":
        return full_corpus(conn, max_chars=max_chars)[: limit * 20]
    if channel == "hybrid":
        return [d for d, _s in hybrid_search(conn, query, limit=limit)]
    if channel == "graph":
        return [d for d, _s in hybrid_search(conn, query, limit=limit, use_graph=True)]
    raise ValueError(f"unknown channel: {channel}")


def fuse_channels(
    tree_ids: list[str], hybrid_ranked: list[tuple[str, float]], k: int = 60
) -> list[tuple[str, float]]:
    """Channels are not mutually exclusive (§6.1): Grader may stack them."""
    ranked = [[(i, 1.0) for i in tree_ids], hybrid_ranked]
    return rrf_fuse(ranked, k=k)
