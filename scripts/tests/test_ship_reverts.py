"""Regression for #1201's soft reset onto a fetched main."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

GUARD = Path(__file__).resolve().parents[1] / "check_ship_reverts.py"


class GuardTest(unittest.TestCase):
    def test_history(self):
        with tempfile.TemporaryDirectory() as directory:
            env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")

            def git(*args):
                return subprocess.check_output(["git", *args], cwd=directory, env=env, stderr=subprocess.DEVNULL)

            def write(name, value):
                Path(directory, name).write_text(value)

            def commit(message):
                git("add", ".")
                git("commit", "-qm", message)

            def guard(*args):
                return subprocess.run(["python3", str(GUARD), *args], cwd=directory, env=env, capture_output=True, text=True)

            git("init", "-qb", "main")
            git("config", "user.email", "test@example.org")
            git("config", "user.name", "Test")
            write("photo settings", "old")
            commit("initial")
            git("branch", "worker")
            git("checkout", "-qb", "fix")
            write("photo settings", "new")
            commit("fix")
            git("checkout", "main")
            git("merge", "--no-ff", "fix", "-m", "Merge #1200")
            git("update-ref", "refs/remotes/origin/main", "HEAD")
            git("checkout", "worker")
            write("worker", "feature")
            commit("worker change")
            self.assertIn("stale base", guard().stderr)
            # The exact failure: old tree becomes a commit on new main.
            git("reset", "--soft", "origin/main")
            commit("bad squash #1201")
            result = guard()
            self.assertEqual(result.returncode, 1)
            self.assertIn("this branch reverts photo settings to before Merge #1200", result.stderr)
            self.assertEqual(guard("--allow-revert", "photo settings").returncode, 0)
            self.assertEqual(guard("--allow-revert", "worker").returncode, 1)
            write("photo settings", "independent improvement")
            commit("legitimate change")
            self.assertEqual(guard().returncode, 0)


if __name__ == "__main__":
    unittest.main()
