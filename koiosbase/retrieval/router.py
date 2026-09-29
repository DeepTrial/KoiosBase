"""Router channels (design doc §6.1). v0.1 ships ② hybrid recall (BM25 + optional
vector) with ③ PPR graph expansion. Channels ⓪ (wiki-first) and ① (tree
navigation) land with the compile layer in v0.3; ④ full-corpus is the Grader
escalation path (§6.2 Self-Route).

P3 — retrieval is navigation + reasoning, not a similarity contest: BM25/vector
are recall channels, never the judge.
"""

from __future__ import annotations

import re
import sqlite3
from collections import defaultdict

CJK_RE = re.compile(r"[\u4e00-\u9fff]")
TOKEN_RE = re.compile(r"[\w\u4e00-\u9fff]+")


def cjk_pad(text: str) -> str:
    """Insert spaces between CJK characters for FTS indexing and querying.

    SQLite's `unicode61` tokenizer treats a RUN of contiguous CJK as ONE token,
    so `年营收32亿元` indexes as a single blob and a query for `营收` matches
    nothing (verified experimentally — see docs). Padding each character with
    spaces makes unicode61 behave like a per-character index, which is what CJK
    retrieval needs. Applied symmetrically to both index and query sides so
    BM25 scoring stays meaningful.
    """
    return CJK_RE.sub(lambda m: " " + m.group(0) + " ", text or "")


# Fusion weights (§6.3) — the graph signal is a bonus, not a dictator.
WEIGHTS = {"alpha": 1.0, "beta": 1.0, "gamma": 0.12, "rrf_k": 60}


def tokenize(text: str) -> list[str]:
    """BM25 vocabulary tokenizer: CJK per character, latin per word.

    Used for scoring vocabulary; see :func:`query_terms` for the *query* side,
    which must keep multi-char CJK terms whole.
    """
    out: list[str] = []
    for tok in TOKEN_RE.findall(text or ""):
        if re.match(r"[\u4e00-\u9fff]", tok):
            out.extend(list(tok))
        else:
            out.append(tok.lower())
    return out


def query_terms(query: str) -> list[str]:
    """Split a query into FTS terms.

    IMPORTANT: unlike :func:`tokenize`, this does NOT explode CJK into single
    characters. SQLite's `unicode61` tokenizer indexes contiguous CJK runs, so
    `营收` matches while `营 AND 收` matches nothing. Keeping multi-char CJK
    terms whole is what makes exact surface matching work for terms and part
    numbers alike (§8.4 vocabulary-level defence).
    """
    return TOKEN_RE.findall(query or "")


def _ch_hit(conn: sqlite3.Connection, block_id: str, chars: list[str]) -> int:
    """How many distinct query characters appear in this block's raw text."""
    row = conn.execute("SELECT raw FROM blocks WHERE id=?", (block_id,)).fetchone()
    if not row:
        return 0
    text = row["raw"] or ""
    return sum(1 for c in set(chars) if c in text)


def fts_search(
    conn: sqlite3.Connection, query: str, limit: int = 20
) -> list[tuple[str, float]]:
    """Channel ② (half): BM25 over the FTS5 mirror.

    Terms are AND-ed first (precision), falling back to OR (recall) so a query
    never collapses to zero just because one term is absent.
    """
    terms = query_terms(query)
    if not terms:
        return []
    # CJK terms must be padded identically to the indexed side: `营收` becomes
    # individual characters so the AND/OR expression actually matches something.
    padded: list[str] = []
    for t in terms:
        if CJK_RE.search(t):
            # A CJK term is matched WHOLE first: per-character padding is only
            # applied as a fallback, because matching on a single common char
            # (e.g. 年) drags in unrelated blocks and breaks the refusal contract.
            padded.append(" ".join(CJK_RE.findall(t)))
        else:
            padded.append(t)
    # Each CJK run stays grouped, so 营收 matches as a phrase and a stray single
    # character cannot create a false positive on its own.
    quoted = [f'"{p}"' for p in padded[:32]]
    # Third-tier fallback: split every CJK run into characters and OR them, which
    # rescues queries whose exact phrase is absent but whose characters are all
    # present (e.g. "负债 18" where only part of the phrase is indexed verbatim).
    chars: list[str] = []
    for p in padded:
        if CJK_RE.search(p):
            chars.extend(CJK_RE.findall(p))
        else:
            chars.append(p)
    char_expr = " OR ".join(f'"{c}"' for c in chars[:64])
    for expr in (" AND ".join(quoted), " OR ".join(quoted), char_expr):
        try:
            rows = conn.execute(
                "SELECT id, bm25(blocks_fts) AS score FROM blocks_fts "
                "WHERE blocks_fts MATCH ? ORDER BY score LIMIT ?",
                (expr, limit),
            ).fetchall()
        except sqlite3.OperationalError:
            continue
        if rows:
            # The per-character tier is deliberately loose; require at least two
            # distinct query characters to hit, otherwise a single ubiquitous
            # character (年) would drag in irrelevant blocks and, downstream, let
            # the escalation path defeat the refusal contract.
            if expr is char_expr and len(chars) > 1:
                rows = [
                    r
                    for r in rows
                    if _ch_hit(conn, r["id"], chars) >= min(2, len(chars))
                ]
            if rows:
                return [(r["id"], float(r["score"])) for r in rows]
    return []


def rrf_fuse(
    rank_lists: list[list[tuple[str, float]]],
    k: int = 60,
    weights: list[float] | None = None,
) -> list[tuple[str, float]]:
    """Reciprocal Rank Fusion — the only safe way to merge incomparable scores."""
    fused: dict[str, float] = defaultdict(float)
    if weights is None:
        weights = [1.0] * len(rank_lists)
    for w, ranks in zip(weights, rank_lists):
        for rank, (doc_id, _score) in enumerate(ranks, start=1):
            fused[doc_id] += w * (1.0 / (k + rank))
    return sorted(fused.items(), key=lambda x: -x[1])


def load_graph(
    conn: sqlite3.Connection, page_confidence: dict[str, str] | None = None
) -> dict[str, list[tuple[str, float]]]:
    """Adjacency lists from the links table (all edges derive from Markdown, §5.2).

    When `page_confidence` is supplied, out-edge weights are multiplied by the
    source page's confidence factor (§6.6: draft = 0). This is what prevents an
    unverified page from conferring authority just by being cited a lot — the
    "citations -> authority -> more citations" self-reinforcing loop.
    """
    from ..compile.gate import edge_weight

    adj: dict[str, list[tuple[str, float]]] = defaultdict(list)
    for r in conn.execute("SELECT src,dst,kind,weight FROM links"):
        w = float(r["weight"])
        if page_confidence:
            w *= edge_weight(page_confidence.get(r["src"], "draft"))
            if w <= 0:
                continue
        adj[r["src"]].append((r["dst"], w))
    return adj


def personalized_pagerank(
    seeds: list[str],
    adj: dict[str, list[tuple[str, float]]],
    damping: float = 0.85,
    iterations: int = 20,
    max_nodes: int = 500,
) -> dict[str, float]:
    """Personalized PageRank over the sub-graph around the candidates (§6.3).

    Only candidates + their <=2-hop neighbourhood are expanded: numpy-free and
    millisecond-scale at top-50 seeds. Global PageRank is deliberately NOT the
    main signal — its tiny static weight only solves cold start.
    """
    if not seeds or not adj:
        return {}
    frontier = set(seeds)
    for _ in range(2):
        nxt = set(frontier)
        for n in list(frontier):
            for dst, _w in adj.get(n, []):
                nxt.add(dst)
        frontier = nxt
        if len(frontier) > max_nodes:
            break
    nodes = list(frontier)[:max_nodes]
    if not nodes:
        return {}
    personal = {n: 1.0 / len(seeds) for n in seeds if n in frontier}
    if not personal:
        return {}
    pr = {n: 1.0 / len(nodes) for n in nodes}
    out_w = {n: sum(w for _d, w in adj.get(n, [])) for n in nodes}
    for _ in range(iterations):
        new = {n: (1.0 - damping) * personal.get(n, 0.0) for n in nodes}
        # Dangling mass is redistributed by the PERSONALIZATION distribution,
        # not uniformly: dumping it uniformly lets sink nodes outrank the seeds,
        # which would invert the whole point of a *personalized* walk.
        dangling = 0.0
        for n in nodes:
            if out_w.get(n, 0.0) <= 0:
                dangling += pr[n]
        for n in nodes:
            total = out_w.get(n, 0.0)
            if total > 0:
                for dst, w in adj.get(n, []):
                    if dst in new:
                        new[dst] += damping * pr[n] * (w / total)
        if dangling > 0:
            for n in nodes:
                new[n] += damping * dangling * personal.get(n, 0.0)
        diff = sum(abs(new[n] - pr[n]) for n in nodes)
        pr = new
        if diff < 1e-6:
            break
    return pr


def hybrid_search(
    conn: sqlite3.Connection,
    query: str,
    limit: int = 20,
    vector_rank: list[tuple[str, float]] | None = None,
    use_graph: bool = True,
) -> list[tuple[str, float]]:
    """Channel ② hybrid recall (+ ③ graph bonus when a link graph exists)."""
    lists = [fts_search(conn, query, limit=limit * 3)]
    weights = [WEIGHTS["beta"]]
    if vector_rank:
        lists.append(vector_rank)
        weights.append(WEIGHTS["alpha"])
    fused = rrf_fuse(lists, k=WEIGHTS["rrf_k"], weights=weights)
    if use_graph:
        adj = load_graph(conn)
        pr = personalized_pagerank([d for d, _ in fused[: min(50, len(fused))]], adj)
        if pr:
            fused = [(d, s + WEIGHTS["gamma"] * pr.get(d, 0.0) * 10) for d, s in fused]
            fused.sort(key=lambda x: -x[1])
    return fused[:limit]
