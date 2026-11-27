#!/usr/bin/env python3
"""Reject stale branch diffs and exact restorations of recent pre-merge blobs."""
import argparse
import subprocess
import sys


def git(*args):
    return subprocess.check_output(["git", *args])


def changed(base):
    revisions = [base] if "..." in base else [base, "HEAD"]
    return set(git("diff", "--name-only", "-z", *revisions).split(b"\0")) - {b""}


def blob(revision, path):
    # ls-tree also represents an absent file, so deletions are checked too.
    return git("--literal-pathspecs", "ls-tree", "-z", revision, "--", path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-revert", action="append", default=[])
    args = parser.parse_args()
    direct = changed("origin/main")
    if direct != changed("origin/main...HEAD"):
        sys.exit("ship: stale base: branch and merge-base file lists differ; rebase onto origin/main and ship again")
    allowed = {p.encode(errors="surrogateescape") for p in args.allow_revert}
    failed = False
    merges = git("rev-list", "--first-parent", "--merges", "-n", "40", "origin/main").splitlines()
    for raw_path in sorted(direct - allowed):
        path = raw_path.decode(errors="surrogateescape")
        current = blob("HEAD", path)
        for merge in merges:
            revision = merge.decode()
            before = blob(revision + "^1", path)
            if before == blob(revision, path) or current != before:
                continue
            subject = git("show", "-s", "--format=%s", revision).decode().strip()
            print(f"ship: this branch reverts {path} to before {subject}; use --allow-revert {path!r} for an intended revert", file=sys.stderr)
            failed = True
            break
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
