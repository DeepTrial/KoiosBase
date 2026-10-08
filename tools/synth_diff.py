#!/usr/bin/env python3
"""Cross-shell differential for the synthesis lazy-compile layer.

Builds ONE fixture, mirrors it, drives the Python side with one copy and the
Rust side with the other, then diffs three things:

  1. page_state rows (key -> value, not counts)
  2. the generated wiki/synthesis/*.md bytes
  3. needs_recompile() answers at each lifecycle step

Exits non-zero on any divergence. Both sides must be run against NON-EMPTY
input — an empty fixture passes trivially and proves nothing.
"""

from __future__ import annotations

import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO))

from koiosbase.compile.synthesis import (  # noqa: E402
    mark_syntheses_stale,
    needs_recompile,
    recompile_synthesis,
)
from koiosbase.state.model import ensure_tables  # noqa: E402
from koiosbase.ingest.pipeline import build_index  # noqa: E402

RUST = REPO / "rust-cli" / "target" / "release" / "koios"

MD = """---
title: 年报
---

# 财务分析
ACME 公司 2024 年营收 32 亿元。
"""

# Rust driver: mirrors the same lifecycle through the Rust port. It reads the
# JSON-ish lines we can diff directly.
RS_DRIVER = r"""
use koios::compile::{mark_syntheses_stale, needs_recompile, recompile_synthesis};
use std::path::Path;

fn main() {
    let v = Path::new(&std::env::args().nth(1).unwrap());
    let conn = koios::connect(v).unwrap();
    let sdir = v.join("wiki").join("synthesis");
    std::fs::create_dir_all(&sdir).unwrap();
    std::fs::write(sdir.join("finance.md"), "# outdated\n").unwrap();
    std::fs::write(sdir.join("risk.md"), "# outdated\n").unwrap();
    std::fs::write(sdir.join("ops.md"), "# outdated\n").unwrap();

    println!("needs_fresh_finance={}", needs_recompile(&conn, v, "finance"));
    println!("needs_missing_ops2={}", needs_recompile(&conn, v, "ops2"));

    let all = mark_syntheses_stale(&conn, &v, None).unwrap();
    println!("all={:?}", all);
    println!("needs_after_all={}", needs_recompile(&conn, v, "finance"));

    // recompile clears only the page it rebuilt
    let target = recompile_synthesis(
        &conn,
        v,
        "finance",
        &["acme".to_string(), "beta".to_string()],
        &[
            ("营收 32 亿".to_string(), "年报.md#财务分析/1".to_string()),
            ("负债 18 亿".to_string(), "年报.md#负债/2".to_string()),
        ],
    )
    .unwrap();
    println!("wrote={}", target.exists());
    println!("needs_after_recompile={}", needs_recompile(&conn, v, "finance"));
    println!("needs_risk_after={}", needs_recompile(&conn, v, "risk"));

    // topics filter: only 'ops' gets flagged now
    conn.execute(
        "UPDATE page_state SET stale=0 WHERE page_path LIKE 'wiki/synthesis/%'",
        [],
    )
    .unwrap();
    let picked = mark_syntheses_stale(&conn, v, Some(&["ops".to_string()])).unwrap();
    println!("picked={:?}", picked);
    println!("needs_ops={}", needs_recompile(&conn, v, "ops"));
    println!("needs_finance_topics={}", needs_recompile(&conn, v, "finance"));
    println!("needs_risk_topics={}", needs_recompile(&conn, v, "risk"));

    // empty topics list must match nothing (Python builds an empty SET, not None)
    conn.execute(
        "UPDATE page_state SET stale=0 WHERE page_path LIKE 'wiki/synthesis/%'",
        [],
    )
    .unwrap();
    let none = mark_syntheses_stale(&conn, v, Some(&[])).unwrap();
    println!("empty_topics={:?}", none);
    println!("needs_ops_after_empty={}", needs_recompile(&conn, v, "ops"));
}
"""


def build_fixture(root: Path) -> Path:
    (root / "raw").mkdir(parents=True)
    (root / "raw" / "r.md").write_text(MD, encoding="utf-8")
    build_index(root)
    n = len(list((root / "raw").rglob("*.md")))
    assert n > 0, "fixture is empty — a differential over nothing proves nothing"
    return root


def python_side(v: Path) -> dict[str, str]:
    out: dict[str, str] = {}
    conn = sqlite3.connect(str(v / ".index" / "tree.db"))
    ensure_tables(conn)
    sdir = v / "wiki" / "synthesis"
    sdir.mkdir(parents=True, exist_ok=True)
    for name in ("finance.md", "risk.md", "ops.md"):
        (sdir / name).write_text("# outdated\n", encoding="utf-8")

    out["needs_fresh_finance"] = str(needs_recompile(conn, v, "finance"))
    out["needs_missing_ops2"] = str(needs_recompile(conn, v, "ops2"))

    out["all"] = str(mark_syntheses_stale(conn, v, None))
    out["needs_after_all"] = str(needs_recompile(conn, v, "finance"))

    recompile_synthesis(
        conn,
        v,
        "finance",
        ["acme", "beta"],
        [("营收 32 亿", "年报.md#财务分析/1"), ("负债 18 亿", "年报.md#负债/2")],
    )
    out["wrote"] = str((sdir / "finance.md").exists())
    out["needs_after_recompile"] = str(needs_recompile(conn, v, "finance"))
    out["needs_risk_after"] = str(needs_recompile(conn, v, "risk"))

    conn.execute("UPDATE page_state SET stale=0 WHERE page_path LIKE 'wiki/synthesis/%'")
    conn.commit()
    out["picked"] = str(mark_syntheses_stale(conn, v, ["ops"]))
    out["needs_ops"] = str(needs_recompile(conn, v, "ops"))
    out["needs_finance_topics"] = str(needs_recompile(conn, v, "finance"))
    out["needs_risk_topics"] = str(needs_recompile(conn, v, "risk"))

    conn.execute("UPDATE page_state SET stale=0 WHERE page_path LIKE 'wiki/synthesis/%'")
    conn.commit()
    out["empty_topics"] = str(mark_syntheses_stale(conn, v, []))
    out["needs_ops_after_empty"] = str(needs_recompile(conn, v, "ops"))
    conn.close()
    return out


def page_states(db: Path) -> dict:
    conn = sqlite3.connect(str(db))
    rows = conn.execute(
        "SELECT page_path,stale,state,reason FROM page_state "
        "WHERE page_path LIKE 'wiki/synthesis/%' ORDER BY page_path"
    ).fetchall()
    conn.close()
    return {r[0]: (r[1], r[2], r[3]) for r in rows}


def main() -> int:
    if not RUST.exists():
        sys.exit(f"{RUST} missing — build first: cargo build --release")

    base = Path(tempfile.mkdtemp(prefix="koios-synthdiff-"))
    py_v = build_fixture(base / "py")
    rs_v = build_fixture(base / "rs")
    nblocks = len(list((rs_v / "raw").rglob("*.md")))
    print(f"fixture: {nblocks} raw doc(s) — non-empty precondition OK")

    py_out = python_side(py_v)

    # compile the Rust driver against the crate, once, out of the way
    drv = base / "driver"
    shutil.copytree(REPO / "rust-cli", drv, ignore=shutil.ignore_patterns("target"))
    (drv / "src" / "bin").mkdir(exist_ok=True)
    bin_src = drv / "src" / "bin" / "synthdiff.rs"
    bin_src.parent.mkdir(parents=True, exist_ok=True)
    bin_src.write_text(RS_DRIVER, encoding="utf-8")
    cargo = ["cargo", "run", "--release", "--quiet", "--bin", "synthdiff", "--", str(rs_v)]
    env = dict(os.environ)
    r = subprocess.run(cargo, cwd=drv, capture_output=True, text=True, env=env)
    if r.returncode != 0:
        print("RUST DRIVER FAILED:\n", r.stderr[-3000:])
        return 1
    rs_out = dict(
        line.split("=", 1) for line in r.stdout.strip().splitlines() if "=" in line
    )

    # --- diff 1: lifecycle answers -------------------------------------------
    fails = 0
    for k in sorted(py_out):
        p, s = py_out[k], rs_out.get(k, "<MISSING>")
        if p != s:
            fails += 1
            print(f"MISMATCH {k}:\n   PY {p}\n   RS {s}")
    print(f"[diff 1] lifecycle answers: {len(py_out) - fails}/{len(py_out)} match")

    # --- diff 2: generated synthesis page bytes ------------------------------
    pyp = (py_v / "wiki" / "synthesis" / "finance.md").read_text(encoding="utf-8")
    rsp = (rs_v / "wiki" / "synthesis" / "finance.md").read_text(encoding="utf-8")
    if pyp != rsp:
        fails += 1
        print("MISMATCH generated page bytes differ:")
        for i, (a, b) in enumerate(zip(pyp.splitlines(), rsp.splitlines())):
            if a != b:
                print(f"   line {i}:\n     PY {a!r}\n     RS {b!r}")
    else:
        print(f"[diff 2] generated page: IDENTICAL ({len(pyp.encode()) } bytes)")

    # --- diff 3: page_state rows (key -> value, not counts) ------------------
    pps, rps = page_states(py_v / ".index" / "tree.db"), page_states(
        rs_v / ".index" / "tree.db"
    )
    if pps != rps:
        fails += 1
        print("MISMATCH page_state rows:")
        for k in sorted(set(pps) | set(rps)):
            if pps.get(k) != rps.get(k):
                print(f"   {k}:\n     PY {pps.get(k)}\n     RS {rps.get(k)}")
    else:
        print(f"[diff 3] page_state rows: {len(pps)} identical")

    print("")
    if fails:
        print(f"RESULT: {fails} divergence(s)")
        return 1
    print("RESULT: IDENTICAL ✔")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
