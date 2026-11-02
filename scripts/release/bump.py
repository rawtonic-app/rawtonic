#!/usr/bin/env python3
"""The version bump of a release (run by scripts/release.sh in the bump worktree).

    scripts/release/bump.py VERSION DATE [--root DIR]

Sets VERSION in package.json, the two top entries of package-lock.json,
src-tauri/Cargo.toml, src-tauri/tauri.conf.json and the `rawtonic-app` rows of
both THIRD_PARTY_NOTICES.md copies, and turns the CHANGELOG's `## Unreleased`
text into `## VERSION — DATE` under a new, empty `## Unreleased`. Workspace
crates stay 0.1.0. Cargo.lock is left to `cargo update -p rawtonic-app --offline`.
Fails without changing anything when a spot is not found or Unreleased is empty.
"""
import argparse
import json
import re
import sys
from pathlib import Path


def bump(root: Path, version: str, date: str) -> dict:
    old = json.loads((root / "package.json").read_text())["version"]
    if old == version:
        raise ValueError(f"the version is {version} already")
    edits = {}

    def sub(rel, pattern, repl, count):
        text = edits.get(rel, (root / rel).read_text())
        new, n = re.subn(pattern, repl, text, count=count)
        if n != count:
            raise ValueError(f"{rel}: expected {count} match(es) of {pattern!r}, found {n}")
        edits[rel] = new

    q = re.escape(old)
    sub("package.json", rf'("version": )"{q}"', rf'\g<1>"{version}"', 1)
    sub("package-lock.json", rf'("version": )"{q}"', rf'\g<1>"{version}"', 2)
    sub("src-tauri/tauri.conf.json", rf'("version": )"{q}"', rf'\g<1>"{version}"', 1)
    sub("src-tauri/Cargo.toml", rf'(\[package\][^\[]*?\nversion = )"{q}"', rf'\g<1>"{version}"', 1)
    for rel in ("THIRD_PARTY_NOTICES.md", "src-tauri/resources/licenses/THIRD_PARTY_NOTICES.md"):
        sub(rel, rf"(\| rawtonic-app \| ){q}( \|)", rf"\g<1>{version}\g<2>", 1)

    changelog = (root / "CHANGELOG.md").read_text()
    m = re.search(r"^## Unreleased\n(.*?)(?=^## )", changelog, re.S | re.M)
    if not m:
        raise ValueError("CHANGELOG.md has no '## Unreleased' section followed by a release")
    text = m.group(1).strip()
    if not text:
        raise ValueError("CHANGELOG.md: '## Unreleased' is empty; write the release's section first")
    if re.search(rf"^## {re.escape(version)} ", changelog, re.M):
        raise ValueError(f"CHANGELOG.md has a {version} section already")
    edits["CHANGELOG.md"] = (
        changelog[: m.start()] + f"## Unreleased\n\n## {version} — {date}\n\n{text}\n\n" + changelog[m.end():]
    )
    return {"old": old, "edits": edits}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("version")
    parser.add_argument("date")
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", args.date):
        sys.exit("bump: DATE must be YYYY-MM-DD")
    try:
        result = bump(args.root, args.version, args.date)
    except (ValueError, OSError, KeyError) as error:
        sys.exit(f"bump: {error}")
    for rel, text in result["edits"].items():
        (args.root / rel).write_text(text)
    print(f"bump: {result['old']} -> {args.version} in {', '.join(sorted(result['edits']))}")


if __name__ == "__main__":
    main()
