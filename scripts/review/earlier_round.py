#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The earlier review round of a branch, for the next round's prompt.

Prints a Markdown section naming the newest stored verdict of an ancestor of
HEAD, or of the same branch after an amend/rebase (scripts/review.sh keeps one per reviewed HEAD), its findings and the
round number, so the reviewer checks those fixes and the commits since then
instead of starting over (AGENTS.md "Review rounds"). Prints nothing on a
first round.
"""
import json
from pathlib import Path
import subprocess
import sys


def is_ancestor(sha, head):
    return sha != head and subprocess.run(
        ["git", "merge-base", "--is-ancestor", sha, head], capture_output=True
    ).returncode == 0


def earlier_rounds(store, head):
    branch = subprocess.check_output(
        ["git", "branch", "--show-current"], text=True
    ).strip()
    rounds = []
    for path in Path(store).glob("*.json") if Path(store).is_dir() else []:
        if path.stem == head:
            continue
        verdict = json.loads(path.read_text())
        stored_branch = verdict.get("branch")
        if ((stored_branch == branch and branch)
                or (stored_branch is None and is_ancestor(path.stem, head))):
            rounds.append((path.stat().st_mtime, path))
    return sorted(rounds)


def main(store, head, previous_only=False):
    rounds = earlier_rounds(store, head)
    if not rounds:
        return
    last = rounds[-1][1]
    if previous_only:
        print(last.stem)
        return
    verdict = json.loads(last.read_text())
    delta = f"{last.stem}...HEAD" if is_ancestor(last.stem, head) else f"{last.stem} HEAD"
    print("\n## Earlier review round\n")
    print(f"This is review round {len(rounds) + 1} of this branch. Round {len(rounds)} reviewed `{last.stem}` "
          f"(verdict `{verdict.get('verdict')}`) with these findings:\n")
    for f in verdict.get("findings", []):
        print(f"- [{f['severity']}] {f['file']}:{f['line']} {f['issue']}")
    print(f"\nCheck that those findings are fixed and review the commits since then "
          f"(`git diff {delta}`). A new finding in code that did not change since "
          f"`{last.stem}` is `should_fix` at most, unless it loses data, alters originals, breaks "
          f"exports or is a security, licence or privacy problem.")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], "--previous" in sys.argv[3:])
