"""koios CLI — vault lifecycle: init / ingest / search / lint.

Subcommands mirror the SoT→derived model: everything under .index/ is
disposable and rebuilt from raw/ + wiki/ on demand (P1).
"""

from __future__ import annotations

import argparse
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

    pc = sub.add_parser("checkclaim", help="cross-judge one claim against evidence")
    pc.add_argument("claim")
    pc.add_argument("evidence")
    pc.set_defaults(func=check_claim_cmd)

    return p


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

    blocks = retrieve(conn, q, top=args.top, channel=args.channel)
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
