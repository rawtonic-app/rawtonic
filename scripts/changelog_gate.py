#!/usr/bin/env python3
"""A change to user-facing code needs a CHANGELOG.md entry (gate of scripts/check.sh).

Compares $(git merge-base $CHECK_BASE HEAD)..HEAD (default $CHECK_BASE:
origin/main) and fails when that diff changes user-facing code but not
CHANGELOG.md. User-facing is decided by path only:

  src/**            except *.test.ts(x) and src/test/
  src-tauri/src/**
  crates/*/src/**
  src-tauri/resources/**   except the licence copies
  src-tauri/tauri.conf.json

Docs, tests, CI files, scripts, vendor/ and third_party/ are not user-facing.

Opt-out: when any commit message on the branch has a line `Changelog: none`
(a refactor with no user-visible effect), the gate passes. On main itself
the diff is empty, so the gate (and CI after the merge) passes.

Exit 0 = pass, 1 = fail, 2 = git could not answer.
"""
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

CHANGELOG = "CHANGELOG.md"
USER_FACING_FILES = {"src-tauri/tauri.conf.json"}
TEST_DIRS = ("src/test/",)
LICENCE_PREFIX = "src-tauri/resources/licenses/"
OPT_OUT = "changelog: none"

MESSAGE = (
    "CHANGELOG.md is not changed but the branch changes user-facing code: "
    "add an entry under '## Unreleased' (or put a line 'Changelog: none' in "
    "a commit message when the change has no user-visible effect)."
)


class GitError(Exception):
    pass


def git(*args):
    try:
        return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True,
                              text=True).stdout
    except (OSError, subprocess.CalledProcessError) as exc:
        detail = getattr(exc, "stderr", "") or exc
        raise GitError(f"could not run git {' '.join(args)}: {str(detail).strip()}") from exc


def is_user_facing(path):
    if path in USER_FACING_FILES:
        return True
    if path.startswith(LICENCE_PREFIX) or path.startswith(TEST_DIRS):
        return False
    if path.startswith("src/"):
        return not path.endswith((".test.ts", ".test.tsx"))
    if path.startswith(("src-tauri/src/", "src-tauri/resources/")):
        return True
    # A crate's sources, not its tests/ and benches/ directories.
    parts = path.split("/")
    return len(parts) >= 4 and parts[0] == "crates" and parts[2] == "src"


def report():
    """(user-facing files changed, entry changed, opt-out) of $CHECK_BASE..HEAD."""
    base = git("merge-base", os.environ.get("CHECK_BASE", "origin/main"), "HEAD").strip()
    files = git("diff", "--no-renames", "--name-only", "-z", f"{base}..HEAD").split("\0")
    user_facing = sorted(f for f in files if f and is_user_facing(f))
    logged = CHANGELOG in files
    messages = git("log", "--format=%B", f"{base}..HEAD")
    opt_out = any(line.strip().lower() == OPT_OUT for line in messages.splitlines())
    return user_facing, logged, opt_out


def main():
    try:
        user_facing, logged, opt_out = report()
    except GitError as exc:
        print(f"changelog gate: {exc}", file=sys.stderr)
        return 2
    if not user_facing or logged or opt_out:
        return 0
    if len(user_facing) == 1:
        print(f"CHANGELOG.md is not changed but the branch changes user-facing code "
              f"({user_facing[0]}): add an entry under '## Unreleased' (or put a line "
              f"'Changelog: none' in a commit message when the change has no "
              f"user-visible effect).", file=sys.stderr)
        return 1
    print(MESSAGE, file=sys.stderr)
    for path in user_facing[:20]:
        print(f"  user-facing: {path}", file=sys.stderr)
    if len(user_facing) > 20:
        print(f"  … and {len(user_facing) - 20} more", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
