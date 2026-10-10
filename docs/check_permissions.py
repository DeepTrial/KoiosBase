#!/usr/bin/env python3
"""Fail if any tracked file has permissions that would break a clone.

Two ways this bites, both of which happened here:

  * 0600 on a tracked file — git preserves the executable bit only, so a clone
    gets 0644 and everything looks fine locally while nobody else can read the
    source you just pushed. Invisible until someone else clones.
  * 0644 on a script the docs tell you to run (`docs/check_diagrams.sh`) — the
    instructions are then wrong for every reader.

The 0600s crept in despite `umask 0002`: files written by tooling do not go
through the shell's umask. Nothing catches that at commit time, so catch it here.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Preserve existing 0755 on files that legitimately need it (shell scripts);
# everything else must be world-readable at 0644.
SCRIPTS = {".sh"}


def tracked_files() -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=REPO, capture_output=True, check=True
    ).stdout.decode()
    return [f for f in out.split("\0") if f]


def main() -> int:
    bad: list[tuple[str, str, str]] = []
    for rel in tracked_files():
        p = REPO / rel
        if not p.is_file() or p.is_symlink():
            continue
        mode = p.stat().st_mode & 0o777
        other_read = bool(mode & 0o004)
        if not other_read:
            bad.append((rel, oct(mode), "not world-readable"))
            continue
        if p.suffix in SCRIPTS and not (mode & 0o111):
            bad.append((rel, oct(mode), "script is not executable"))
    if not bad:
        print(f"permissions: {len(tracked_files())} tracked files OK")
        return 0
    print("permissions: files that would break a clone:")
    for rel, mode, why in bad:
        print(f"  {mode:>6}  {rel}  ({why})")
    print(f"\nfix:  chmod 644 <file>   # or 755 for scripts")
    return 1


if __name__ == "__main__":
    sys.exit(main())
