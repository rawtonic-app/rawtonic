#!/usr/bin/env python3
"""Reject exact restorations of recent pre-merge blobs in branch-owned files."""
import argparse
import subprocess
import sys


def git(*args):
    return subprocess.check_output(["git", *args])


def changed(base):
    revisions = [base] if "..." in base else [base, "HEAD"]
    return set(git("diff", "--name-only", "-z", *revisions).split(b"\0")) - {b""}


def blobs(revision):
    result = {}
    for entry in git("ls-tree", "-rz", revision).split(b"\0"):
        if entry:
            metadata, path = entry.split(b"\t", 1)
            result[path] = metadata.split()[-1]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-revert", action="append", default=[])
    args = parser.parse_args()
    direct = changed("origin/main...HEAD")
    allowed = {p.encode(errors="surrogateescape") for p in args.allow_revert}
    head = blobs("HEAD")
    main_tree = blobs("origin/main")
    pending = {p for p in direct - allowed if head.get(p) != main_tree.get(p)}
    failed = False
    merges = git("rev-list", "--first-parent", "--merges", "-n", "40", "origin/main").splitlines()
    for merge in merges:
        if not pending:
            break
        revision = merge.decode()
        before = blobs(revision + "^1")
        after = blobs(revision)
        for raw_path in sorted(pending):
            if before.get(raw_path) == after.get(raw_path) or head.get(raw_path) != before.get(raw_path):
                continue
            path = raw_path.decode(errors="surrogateescape")
            subject = git("show", "-s", "--format=%s", revision).decode().strip()
            print(f"ship: this branch reverts {path} to before {subject}; use --allow-revert {path!r} for an intended revert", file=sys.stderr)
            failed = True
            pending.remove(raw_path)
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
