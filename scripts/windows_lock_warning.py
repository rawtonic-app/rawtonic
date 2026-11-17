#!/usr/bin/env python3
"""Warn when the Cargo-generated lockfile changes a Windows-dependent package."""

import os
import re
from pathlib import Path

from changed_scope import git


def windows_packages(lockfile):
    # Cargo lists dependencies as "name" or "name version (source)".
    # Keep the full package record so version, source, checksum and dependency
    # changes all count, even when there are multiple versions of a package.
    return {
        package.strip()
        for package in lockfile.split("[[package]]\n")[1:]
        if re.search(r'^ "windows(?: [0-9][^"]*)?",$', package, re.MULTILINE)
    }


def main():
    base = git("merge-base", os.environ.get("CHECK_BASE", "origin/main"), "HEAD").strip()
    if not git("diff", "--name-only", base, "--", "Cargo.lock").strip():
        return

    before = windows_packages(git("show", f"{base}:Cargo.lock"))
    after = windows_packages(Path("Cargo.lock").read_text(encoding="utf-8"))
    changed = before ^ after
    if changed:
        names = sorted({re.search(r'^name = "([^"]+)"$', p, re.MULTILINE)[1] for p in changed})
        print(f"warning: Cargo.lock changed packages that depend on windows ({', '.join(names)}); "
              "check the Quality gates / windows-app job.")


if __name__ == "__main__":
    main()
