"""koios CLI — vault lifecycle: init / ingest / search / lint.

Subcommands mirror the SoT→derived model: everything under .index/ is
disposable and rebuilt from raw/ + wiki/ on demand (P1).
"""

from __future__ import annotations

import argparse
import sqlite3
import sys
from pathlib import Path

from .index.schema import connect
from .ingest.pipeline import cmd_ingest, cmd_init
from .lint.gardener import check_claim_cmd, cmd_lint

__all__ = ["main"]


def _ns_to_argv(args, keys=("path", "claim", "evidence")):
    return [getattr(args, k) for k in keys if getattr(args, k, None) is not None]


def cmd_ingest_wrapper(args) -> int:
    return cmd_ingest(_ns_to_argv(args, ("path",)))


def cmd_lint_wrapper(args) -> int:
    return cmd_lint(_ns_to_argv(args, ("path",)))


def cmd_init_wrapper(args) -> int:
    return cmd_init(_ns_to_argv(args, ("path",)))


def parse_groups(args) -> set[str] | None:
    """`--groups a,b` → the principal's set; absent ⇒ anonymous (never "skip").

    Anonymous is a real principal that sees only public documents — it must NOT
    default to a no-op check, which is how restricted docs leaked (§9.2).
    """
    raw = getattr(args, "groups", None)
    if not raw:
        return None
    parts = [p.strip() for p in raw.split(",") if p.strip()]
    return set(parts) or None


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="koios", description="KoiosBase CLI")
    sub = p.add_subparsers(dest="cmd", required=True)

    pi = sub.add_parser("init", help="initialize a KoiosBase vault")
    pi.add_argument("path", nargs="?", default=".")
    pi.set_defaults(func=cmd_init_wrapper)

    pg = sub.add_parser("index", help="(re)build the derived index from raw/ + wiki/")
    pg.add_argument("path", nargs="?", default=".")
    pg.add_argument("--full", action="store_true", help="force full rebuild")
    pg.set_defaults(func=cmd_ingest_wrapper)

    ps = sub.add_parser("search", help="query the vault")
    ps.add_argument("query", nargs="+")
    # NOTE: -p is explicit so the greedy nargs="+" query cannot swallow the path
    ps.add_argument("-p", "--path", default=".", help="vault directory")
    ps.add_argument("-k", "--top", type=int, default=8)
    ps.add_argument(
        "-c",
        "--channel",
        default=None,
        choices=["hybrid", "tree", "full", "graph"],
        help="force a retrieval channel (§6.1)",
    )
    ps.add_argument("--groups", help="principal groups for ACL (§9.2), comma separated")
    ps.set_defaults(func=lambda a: cmd_search(a))

    pl = sub.add_parser("lint", help="run the knowledge-health gardener")
    pl.add_argument("path", nargs="?", default=".")
    pl.set_defaults(func=cmd_lint_wrapper)

    pe = sub.add_parser("eval", help="run the >=200-question eval baseline (§10.2)")
    pe.add_argument("-p", "--path", default=".", help="vault directory")
    pe.add_argument("-k", "--top", type=int, default=5)
    pe.add_argument(
        "--strict",
        action="store_true",
        help="fail on needs-llm cases too (off by default: those are "
        "known v0.1 gaps awaiting the cross-family reranker)",
    )
    pe.set_defaults(func=cmd_eval)

    pc = sub.add_parser("compile", help="compile entities/ from raw (§5.3)")
    pc.add_argument("-p", "--path", default=".", help="vault directory")
    pc.set_defaults(func=cmd_compile)

    pp = sub.add_parser("promote", help="promote a compiled page's confidence (§5.4)")
    pp.add_argument("page", help="path to the wiki page")
    pp.add_argument(
        "--verified", action="store_true", help="cross-family verification passed"
    )
    pp.add_argument("--human", action="store_true", help="human confirmed")
    pp.set_defaults(func=cmd_promote)

    pa = sub.add_parser(
        "answer", help="write an approved answer back to wiki/answers/ (§6.6)"
    )
    pa.add_argument("-p", "--path", default=".", help="vault directory")
    pa.add_argument("-q", "--question", required=True)
    pa.add_argument("-a", "--answer", required=True)
    pa.set_defaults(func=cmd_answer)

    pr = sub.add_parser(
        "retract", help="retract a block and cascade to citing pages (§8.3)"
    )
    pr.add_argument("-p", "--path", default=".", help="vault directory")
    pr.add_argument("-b", "--block", required=True, help="block id to retract")
    pr.add_argument("-r", "--reason", default="", help="why (recorded in the cascade)")
    pr.add_argument("--groups", help="principal groups for ACL (§9.2), comma separated")
    pr.set_defaults(func=cmd_retract)

    ps = sub.add_parser("mcp", help="run the MCP server over stdio (§11)")
    ps.add_argument("-p", "--path", default=".", help="default vault for tools")
    ps.set_defaults(func=cmd_mcp)

    pst = sub.add_parser("studio", help="Studio-style export: brief | mindmap (§13)")
    pst.add_argument("kind", choices=["brief", "mindmap"])
    pst.add_argument("-p", "--path", default=".", help="vault directory")
    pst.add_argument("-t", "--topic", required=True)
    pst.add_argument("--groups", help="principal groups for ACL (§9.2), comma separated")
    pst.set_defaults(func=cmd_studio)

    pc2 = sub.add_parser("checkclaim", help="cross-judge one claim against evidence")
    pc2.add_argument("claim")
    pc2.add_argument("evidence")
    pc2.set_defaults(func=check_claim_cmd)

    return p


def cmd_mcp(args) -> int:
    """Serve MCP over stdio (§11). See koiosbase.mcp.server for the protocol."""
    from .mcp.server import serve

    return serve()


def cmd_studio(args) -> int:
    """Generate a Studio-style export (§13) from knowledge already in the vault."""
    from .index.schema import connect
    from .studio.exports import studio_brief, studio_mindmap

    vault = Path(args.path).resolve()
    conn = connect(vault / ".index")
    conn.row_factory = sqlite3.Row
    groups = parse_groups(args)
    if args.kind == "brief":
        path = studio_brief(conn, vault, args.topic, principal_groups=groups)
    else:
        path = studio_mindmap(conn, vault, args.topic, principal_groups=groups)
    print(f"wrote {path}")
    conn.close()
    return 0


def cmd_retract(args) -> int:
    """Retract a block and mark every citing page stale (§8.3).

    This is the precise surgery the three-layer model buys: the blast radius is a
    reverse-index lookup, not a re-chunk-and-re-embed of the whole library.
    """
    from .index.schema import connect
    from .state.model import cascade_retraction

    vault = Path(args.path).resolve()
    conn = connect(vault / ".index")
    res = cascade_retraction(conn, args.block, args.reason, vault=vault)
    print(
        f"retracted {res['block']}; affected pages: "
        f"{len(res['affected_pages'])} {res['affected_pages']}"
    )
    conn.close()
    return 0


def cmd_compile(args) -> int:
    """Compile entity/synthesis pages from the indexed raw layer (§5.3)."""
    from .compile.pipeline import compile_vault

    summary = compile_vault(Path(args.path).resolve())
    print("compiled: " + " ".join(f"{k}={v}" for k, v in summary.items()))
    return 0


def cmd_promote(args) -> int:
    """Promote one step; only via verification or human confirmation (§5.4)."""
    from .compile.gate import promote, read_confidence

    page = Path(args.page)
    before = read_confidence(page)
    after = promote(page, verified=args.verified, human_confirmed=args.human)
    if after == before:
        print(f"not promoted (still {before}): needs --verified or --human")
        return 1
    print(f"promoted: {before} -> {after}")
    return 0


def cmd_answer(args) -> int:
    """Answer write-back: the approved answer becomes a draft page (§6.6)."""
    from datetime import datetime, timezone

    from .compile.gate import write_answer_page
    from .index.schema import connect
    from .query.pipeline import retrieve

    vault = Path(args.path).resolve()
    conn = connect(vault / ".index")
    evidence = retrieve(conn, args.question, top=5)
    path = write_answer_page(
        vault,
        args.question,
        args.answer,
        evidence,
        datetime.now(timezone.utc).date().isoformat(),
    )
    print(f"wrote {path} (confidence: draft, {len(evidence)} evidence links)")
    return 0


def cmd_eval(args) -> int:
    """Run the seed golden set and report the §10.2 metrics."""
    from .eval.harness import run_eval
    from .index.schema import connect

    conn = connect(Path(args.path) / ".index")
    rep = run_eval(conn, top=args.top)
    print(
        f"total={rep.total} recall@1={rep.recall_at_1:.3f} "
        f"refusal_acc={rep.refusal_accuracy:.3f} "
        f"citation_cov={rep.citation_coverage:.3f} "
        f"needs_llm={rep.skipped_needs_llm}"
    )
    blocking = (
        rep.failures
        if args.strict
        else [f for f in rep.failures if not f.startswith("[needs-llm]")]
    )
    for f in blocking:
        print(f"  FAIL {f}")
    for f in rep.failures:
        if f.startswith("[needs-llm]"):
            print(f"  SKIP {f}")
    conn.close()
    return 1 if blocking else 0


def cmd_search(args) -> int:
    vault = Path(args.path)
    conn = connect(vault / ".index")
    q = " ".join(args.query)
    from .query.pipeline import retrieve

    blocks = retrieve(
        conn, q, top=args.top, channel=args.channel, principal_groups=parse_groups(args)
    )
    if not blocks:
        print("(no hits)")
        return 0
    for i, b in enumerate(blocks):
        snippet = b["raw"].replace("\n", " ")[:110]
        print(f"[{i}] {b['id']}\n    {b['breadcrumb']}\n    {snippet}")
    return 0


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    return int(args.func(args) or 0)


if __name__ == "__main__":
    sys.exit(main())
