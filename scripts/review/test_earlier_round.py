#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Checks for the earlier-round section of the review prompt."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HELPER = Path(__file__).with_name("earlier_round.py").resolve()


class EarlierRoundTest(unittest.TestCase):
    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root).decode().strip()

    def commit(self, name):
        (self.root / name).write_text(name)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", name)
        return self.git("rev-parse", "HEAD")

    def verdict(self, sha, mtime, findings, branch=None):
        path = self.store / f"{sha}.json"
        path.write_text(json.dumps({"verdict": "changes_requested", "findings": findings,
                                    **({"branch": branch} if branch is not None else {})}))
        os.utime(path, (mtime, mtime))

    def run_helper(self, head):
        return subprocess.check_output([sys.executable, str(HELPER), str(self.store), head], cwd=self.root).decode()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "repo"
        self.root.mkdir()
        self.git("init", "-q")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "test")
        self.store = Path(self.temp.name) / "store"
        self.store.mkdir()

    def test_first_round_prints_nothing(self):
        head = self.commit("a")
        self.assertEqual(self.run_helper(head), "")

    def test_names_the_newest_ancestor_round_and_its_findings(self):
        a = self.commit("a")
        b = self.commit("b")
        head = self.commit("c")
        self.verdict(a, 1000, [{"severity": "blocking", "file": "x.ts", "line": 1, "issue": "old"}])
        self.verdict(b, 2000, [{"severity": "blocking", "file": "y.ts", "line": 2, "issue": "race"}])
        self.verdict(head, 3000, [])  # HEAD's own verdict is not an earlier round
        out = self.run_helper(head)
        self.assertIn("review round 3", out)
        self.assertIn(f"`{b}`", out)
        self.assertIn("[blocking] y.ts:2 race", out)
        self.assertIn(f"git diff {b}...HEAD", out)
        self.assertNotIn("old", out)

    def test_ignores_verdicts_of_other_branches(self):
        base = self.commit("a")
        self.git("checkout", "-q", "-b", "other")
        other = self.commit("b")
        self.git("checkout", "-q", "-")
        head = self.commit("c")
        self.verdict(other, 1000, [{"severity": "blocking", "file": "z", "line": 3, "issue": "elsewhere"}])
        self.assertEqual(self.run_helper(head), "")
        self.verdict(base, 500, [])
        self.assertIn("review round 2", self.run_helper(head))

    def test_rebase_keeps_branch_rounds_and_uses_endpoint_diff(self):
        base = self.commit("base")
        self.git("checkout", "-q", "-b", "topic")
        first = self.commit("a")
        self.verdict(first, 1000, [], branch="topic")
        second = self.commit("b")
        self.verdict(second, 2000, [
            {"severity": "blocking", "file": "b", "line": 1, "issue": "fix me"}
        ], branch="topic")
        self.git("checkout", "-q", "-b", "upstream", base)
        self.commit("upstream")
        self.git("checkout", "-q", "topic")
        self.git("rebase", "upstream")
        head = self.commit("fix")
        out = self.run_helper(head)
        self.assertIn("review round 3", out)
        self.assertIn("fix me", out)
        self.assertIn(f"git diff {second} HEAD", out)
        previous = subprocess.check_output(
            [sys.executable, str(HELPER), str(self.store), head, "--previous"],
            cwd=self.root, text=True,
        ).strip()
        self.assertEqual(previous, second)

    def test_amend_keeps_round_but_other_branch_metadata_is_ignored(self):
        self.commit("base")
        self.git("checkout", "-q", "-b", "topic")
        previous = self.commit("a")
        self.verdict(previous, 1000, [], branch="topic")
        (self.root / "a").write_text("fixed")
        self.git("add", "a")
        self.git("commit", "--amend", "-q", "--no-edit")
        head = self.git("rev-parse", "HEAD")
        self.assertIn("review round 2", self.run_helper(head))
        self.verdict(previous, 2000, [], branch="different")
        self.assertEqual(self.run_helper(head), "")


if __name__ == "__main__":
    unittest.main()
