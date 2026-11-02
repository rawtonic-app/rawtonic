#!/usr/bin/env python3
"""Tests of scripts/changelog_gate.py against throwaway git repositories.

Run: python3 scripts/changelog_gate_test.py
"""
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

GATE = Path(__file__).resolve().parent / "changelog_gate.py"
# A clean environment for git: no host config (hooks, signing) and no
# GIT_* variables that point at another repository.
ENV = {
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_AUTHOR_NAME": "Test",
    "GIT_AUTHOR_EMAIL": "test@example.invalid",
    "GIT_COMMITTER_NAME": "Test",
    "GIT_COMMITTER_EMAIL": "test@example.invalid",
}


class GateTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.git("init", "-q", "-b", "feature")
        # The gate's own copy inside the throwaway repo: ROOT (and so the
        # repository the gate reads) comes from the script's location.
        self.gate_path = self.dir / "gate" / "changelog_gate.py"
        self.gate_path.parent.mkdir()
        shutil.copy(GATE, self.gate_path)
        self.write("CHANGELOG.md", "# Changelog\n\n## Unreleased\n")
        self.write("src/app.ts", "export const x = 1\n")
        self.commit("initial")
        # The gate compares with the merge base of origin/main; a local branch
        # stands in for the origin remote.
        self.git("branch", "origin/main")

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.dir, env=ENV, check=True, capture_output=True, text=True)

    def write(self, path, text):
        target = self.dir / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def gate(self):
        return subprocess.run(
            [sys.executable, str(self.gate_path)], cwd=self.dir, env=ENV,
            capture_output=True, text=True, check=False)

    def assertFail(self, *expected):
        result = self.gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        message, *listed = result.stderr.splitlines()
        self.assertIn("CHANGELOG.md is not changed", message)
        self.assertIn("Changelog: none", message)
        # One changed user-facing file is named in the message itself.
        if len(expected) == 1:
            self.assertEqual(listed, [], result.stderr)
            self.assertIn(expected[0], message)
        else:
            for path in expected:
                self.assertIn(f"  user-facing: {path}", listed)

    def assertPass(self):
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")

    def test_user_facing_change_without_changelog_fails(self):
        self.write("src/app.ts", "export const x = 2\n")
        self.commit("feat: change the app")
        self.assertFail("src/app.ts")

    def test_each_user_facing_path_fails_alone(self):
        for path in ("src-tauri/src/lib.rs", "crates/engine/src/lib.rs",
                     "src-tauri/resources/presets/p.json", "src-tauri/tauri.conf.json"):
            with self.subTest(path=path):
                self.write(path, "{}\n")
                self.commit(f"feat: {path}")
                result = self.gate()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(path, result.stderr)

    def test_user_facing_change_with_changelog_passes(self):
        self.write("src/app.ts", "export const x = 2\n")
        self.write("CHANGELOG.md", "# Changelog\n\n## Unreleased\n\n- Faster import.\n")
        self.commit("feat: change the app, with notes")
        self.assertPass()

    def test_only_docs_pass(self):
        self.write("docs/guide.md", "more\n")
        self.write("README.md", "more\n")
        self.commit("docs: guide")
        self.assertPass()

    def test_tests_scripts_ci_and_vendored_pass(self):
        for path in ("src/app.test.ts", "src/app.test.tsx", "src/test/helpers.ts",
                     "crates/engine/tests/api.rs", "tests/e2e.rs", "scripts/x.sh",
                     ".github/workflows/ci.yml", "vendor/paste/src/lib.rs",
                     "third_party/rawler/src/lib.rs",
                     "src-tauri/resources/licenses/AGPL-3.0.txt"):
            with self.subTest(path=path):
                self.write(path, "x\n")
        self.commit("test: more tests")
        self.assertPass()

    def test_test_files_do_not_mask_a_user_facing_change(self):
        self.write("src/app.test.ts", "x\n")
        self.write("src/hooks/useThumbnails.ts", "x\n")
        self.commit("feat: thumbs")
        result = self.gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("src/hooks/useThumbnails.ts", result.stderr)
        self.assertNotIn("src/app.test.ts", result.stderr)

    def test_opt_out_line_passes(self):
        self.write("src/app.ts", "export const x = 3\n")
        self.commit("refactor: split app\n\nChangelog: none")
        self.assertPass()

    def test_opt_out_must_be_a_line(self):
        self.write("src/app.ts", "export const x = 3\n")
        self.commit("refactor: split app (Changelog: none)")
        self.assertFail("src/app.ts")

    def test_no_diff_passes(self):
        self.assertPass()

    def test_uncommitted_changes_do_not_count(self):
        self.write("src/app.ts", "export const x = 4\n")
        self.assertPass()

    def test_branch_point_is_the_merge_base(self):
        # main moves on after the branch: only the branch's own commits count.
        self.git("checkout", "-q", "origin/main")
        self.write("src/main_only.ts", "x\n")
        self.commit("feat: main only")
        self.git("checkout", "-q", "feature")
        self.assertPass()
        self.write("src/app.ts", "export const x = 5\n")
        self.commit("feat: app")
        self.assertFail("src/app.ts")


if __name__ == "__main__":
    unittest.main()
