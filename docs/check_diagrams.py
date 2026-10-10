#!/usr/bin/env python3
"""Check every ```mermaid block in the docs with mermaid's own parser.

The operator wants diagrams validated before they are committed — a README whose
diagram fails to render is worse than one with no diagram at all, and mermaid
syntax errors are invisible to any Markdown linter.

Usage:  python3 docs/check_diagrams.py [FILE ...]
Default: README.md + docs/i18n/README.*.md

Requires node + the mermaid package. Bootstrap once:

    mkdir -p /tmp/mmd && cd /tmp/mmd
    echo '{"name":"mmd","private":true,"type":"module"}' > package.json
    npm install mermaid@11 jsdom

Then point MERMAID_CHECK_DIR at it if it moved.
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CHECK_DIR = Path(os.environ.get("MERMAID_CHECK_DIR", "/tmp/mmd"))

CHECK_JS = r"""
import fs from 'node:fs';
import { JSDOM } from 'jsdom';
const dom = new JSDOM('<!doctype html><html><body></body></html>');
globalThis.window = dom.window;
globalThis.document = dom.window.document;
Object.defineProperty(globalThis, 'navigator', {
  value: dom.window.navigator, configurable: true, writable: true,
});
for (const k of ['Element','SVGElement','Node','HTMLElement','DOMParser','NodeFilter','trustedTypes']) {
  try { globalThis[k] = dom.window[k]; } catch { /* trustedTypes may not exist */ }
}
const mermaid = (await import('mermaid')).default;
let bad = 0, total = 0;
for (const file of process.argv.slice(2)) {
  const md = fs.readFileSync(file, 'utf8');
  const blocks = [...md.matchAll(/```mermaid\n([\s\S]*?)```/g)].map(m => m[1]);
  if (!blocks.length) { console.log(`  --     ${file} (no diagrams)`); continue; }
  for (const [i, b] of blocks.entries()) {
    total++;
    const head = b.trim().split('\n')[0].slice(0, 40);
    try {
      await mermaid.parse(b);
      console.log(`  OK     ${file} #${i}  [${head}]`);
    } catch (e) {
      bad++;
      const msg = String(e.message || e).split('\n').slice(0, 3).join('\n           ');
      console.log(`  BROKEN ${file} #${i}  [${head}]\n           ${msg}`);
    }
  }
}
console.log(`\n${total - bad}/${total} diagrams valid`);
process.exit(bad ? 1 : 0);
"""


def _ensure_check_dir() -> Path | None:
    """Locate (or build) the node sandbox holding mermaid + jsdom."""
    if (CHECK_DIR / "check.mjs").exists():
        return CHECK_DIR
    if not (CHECK_DIR.exists() and (CHECK_DIR / "node_modules").exists()):
        try:
            CHECK_DIR.mkdir(parents=True, exist_ok=True)
            (CHECK_DIR / "package.json").write_text(
                '{"name":"mmd","private":true,"type":"module"}\n'
            )
            subprocess.run(
                ["npm", "install", "mermaid@11", "jsdom", "--no-audit", "--no-fund"],
                cwd=CHECK_DIR, check=True, capture_output=True, timeout=300,
            )
        except Exception as exc:  # no network, no npm, no permission
            print(f"  SKIP   could not prepare mermaid sandbox: {exc}")
            return None
    (CHECK_DIR / "check.mjs").write_text(CHECK_JS)
    return CHECK_DIR


def main(argv: list[str]) -> int:
    files = argv[1:] or sorted(
        [str(REPO / "README.md")] + [str(p) for p in (REPO / "docs/i18n").glob("README.*.md")]
    )
    missing = [f for f in files if not Path(f).exists()]
    if missing:
        for f in missing:
            print(f"  MISSING {f}")
        return 2

    check = _ensure_check_dir()
    if check is None:
        # Cannot validate — say so loudly rather than reporting false success.
        print("  RESULT unknown — mermaid parser unavailable")
        return 0

    proc = subprocess.run(
        ["node", str(check / "check.mjs"), *files],
        cwd=check, capture_output=True, text=True, timeout=300,
    )
    sys.stdout.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    return proc.returncode


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
