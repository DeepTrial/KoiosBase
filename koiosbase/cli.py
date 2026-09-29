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
    ps.set_defaults(func=lambda a: cmd_search(a))

    pl = sub.add_parser("lint", help="run the knowledge-health gardener")
    pl.add_argument("path", nargs="?", default=".")
    pl.set_defaults(func=cmd_lint_wrapper)

    pc = sub.add_parser("checkclaim", help="cross-judge one claim against evidence")
    pc.add_argument("claim")
    pc.add_argument("evidence")
    pc.set_defaults(func=check_claim_cmd)

    return p


def cmd_search(args) -> int:
    vault = Path(args.path)
    conn = connect(vault / ".index")
    q = " ".join(args.query)
    from .retrieval.router import hybrid_search

    rows = hybrid_search(conn, q, limit=args.top)
    if not rows:
        print("(no hits)")
        return 0
    for bid, score in rows:
        row = conn.execute(
            "SELECT breadcrumb,raw FROM blocks WHERE id=?", (bid,)
        ).fetchone()
        if not row:
            continue
        snippet = row["raw"].replace("\n", " ")[:110]
        print(f"[{score:.4f}] {bid}\n    {row['breadcrumb']}\n    {snippet}")
    return 0


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    return int(args.func(args) or 0)


if __name__ == "__main__":
    sys.exit(main())
