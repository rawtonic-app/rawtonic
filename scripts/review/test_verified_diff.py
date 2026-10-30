#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Security regression checks for the review input's upstream equivalence proof."""
import hashlib
import io
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

HELPER = Path(__file__).with_name("verified_diff.py").resolve()
VENDOR = Path("vendor/libsqlite3-sys")


class VerifiedDiffTest(unittest.TestCase):
    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, stderr=subprocess.PIPE)

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "fixture")
        return self.git("rev-parse", "HEAD").decode().strip()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "repo"
        self.root.mkdir()
        self.archive = Path(self.temp.name) / "pinned.crate"
        self.original = {
            "Cargo.toml": b'[package]\nname = "libsqlite3-sys"\nversion = "0.38.2"\n',
            "sqlite3/sqlite3.c": b"int old_lock_transition;\n",
            "src/lib.rs": b"// verified upstream sentinel\n",
            "LICENSE": b"upstream license retained\n",
        }
        with tarfile.open(self.archive, "w:gz") as package:
            for name, data in self.original.items():
                info = tarfile.TarInfo("libsqlite3-sys-0.38.2/" + name)
                info.size, info.mode = len(data), 0o644
                package.addfile(info, io.BytesIO(data))
        checksum = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        (self.root / "Cargo.lock").write_text(
            'version = 3\n[[package]]\nname = "libsqlite3-sys"\nversion = "0.38.2"\n'
            'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
            f'checksum = "{checksum}"\n'
        )
        (self.root / "app.rs").write_text("old app code\n")
        self.git("init", "-q")
        self.git("config", "user.name", "Review Fixture")
        self.git("config", "user.email", "fixture@users.noreply.github.com")
        # Only this disposable fixture repository has no hooks; no project
        # hook is bypassed and this repository has no remote or push.
        self.git("config", "core.hooksPath", "/dev/null")
        self.git("config", "core.fileMode", "true")
        self.base = self.commit()
        for name, data in self.original.items():
            path = self.root / VENDOR / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        (self.root / VENDOR / "sqlite3/sqlite3.c").write_text("int corrected_lock_transition;\n")
        (self.root / "app.rs").write_text("new app code\n")
        self.commit()

    def run_helper(self):
        return subprocess.run(
            [sys.executable, str(HELPER), "--base", self.base, "--archive", str(self.archive)],
            cwd=self.root, capture_output=True, text=True,
        )

    def test_exact_base_after_amend_excludes_unchanged_branch_work(self):
        previous = self.git("rev-parse", "HEAD").decode().strip()
        (self.root / "app.rs").write_text("fixed app code\n")
        self.git("add", "-A")
        self.git("commit", "--amend", "-q", "--no-edit")
        result = subprocess.run(
            [sys.executable, str(HELPER), "--base", previous, "--exact-base"],
            cwd=self.root, capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-new app code", result.stdout)
        self.assertIn("+fixed app code", result.stdout)
        self.assertNotIn("sqlite3", result.stdout)

    def test_only_proven_upstream_bytes_are_elided_and_app_diff_remains(self):
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("+int corrected_lock_transition;", result.stdout)
        self.assertIn("-int old_lock_transition;", result.stdout)
        self.assertIn("+new app code", result.stdout)
        self.assertNotIn("verified upstream sentinel", result.stdout)
        self.assertIn(hashlib.sha256(self.archive.read_bytes()).hexdigest(), result.stdout)

    def test_tampered_archive_fails_without_emitting_a_partial_review(self):
        self.archive.write_bytes(self.archive.read_bytes() + b"tampered")
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("merge-base checksum", result.stderr)

    def test_proposed_lock_checksum_cannot_bless_another_upstream(self):
        self.archive.write_bytes(self.archive.read_bytes() + b"tampered")
        lock = self.root / "Cargo.lock"
        text = lock.read_text()
        old = text.split('checksum = "')[1].split('"')[0]
        lock.write_text(text.replace(old, hashlib.sha256(self.archive.read_bytes()).hexdigest()))
        self.commit()
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

    def test_uncommitted_worktree_cannot_replace_the_reviewed_head(self):
        (self.root / VENDOR / "sqlite3/sqlite3.c").write_text("uncommitted replacement\n")
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("corrected_lock_transition", result.stdout)
        self.assertNotIn("uncommitted replacement", result.stdout)

    def test_new_code_and_removed_upstream_license_are_reported(self):
        (self.root / VENDOR / "build.rs").write_text("unexpected build code\n")
        (self.root / VENDOR / "LICENSE").unlink()
        self.commit()
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("+unexpected build code", result.stdout)
        self.assertIn("-upstream license retained", result.stdout)

    def test_symlink_cannot_masquerade_as_verified_source(self):
        path = self.root / VENDOR / "src/lib.rs"
        path.unlink()
        path.symlink_to("../../../../app.rs")
        self.commit()
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("non-regular", result.stderr)

    def test_executable_mode_change_is_reported_even_when_bytes_match(self):
        # Set the committed Git mode directly, also on hosts whose file
        # system cannot represent a Unix executable permission bit.
        self.git("update-index", "--chmod=+x", str(VENDOR / "src/lib.rs"))
        self.git("commit", "-q", "-m", "fixture executable mode")
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("mode 100644 -> 100755", result.stdout)

    def test_existing_vendor_change_uses_the_complete_normal_git_diff(self):
        self.base = self.git("rev-parse", "HEAD").decode().strip()
        (self.root / VENDOR / "src/lib.rs").write_text("later source change\n")
        self.commit()
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-// verified upstream sentinel", result.stdout)
        self.assertIn("+later source change", result.stdout)
        self.assertNotIn("verified and elided", result.stdout)

    def test_unsupported_lock_syntax_is_rejected_not_interpreted_differently(self):
        # A base must use the generated lockfile format, not escaped TOML
        # that another parser could interpret as a second package name.
        lock = self.root / "Cargo.lock"
        lock.write_text(lock.read_text().replace('name = "libsqlite3-sys"', 'name = "\\u006cibsqlite3-sys"'))
        self.base = self.commit()
        # The new base already contains vendor files: create another fixture
        # base with the same malformed pin but without a vendor directory.
        self.git("rm", "-r", str(VENDOR))
        self.base = self.commit()
        for name, data in self.original.items():
            path = self.root / VENDOR / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.commit()
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("unsupported Cargo.lock", result.stderr)

    def test_new_code_without_final_newline_still_has_a_visible_diff(self):
        (self.root / VENDOR / "build.rs").write_bytes(b"untrusted build code")
        self.commit()
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("+untrusted build code\n", result.stdout)
        self.assertIn("No final newline in proposed file", result.stdout)

    def test_line_ending_changes_are_not_elided(self):
        (self.root / VENDOR / "src/lib.rs").write_bytes(b"// verified upstream sentinel\r\n")
        self.commit()
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("CRLF line endings: upstream 0, proposed 1", result.stdout)
        self.assertIn("-// verified upstream sentinel", result.stdout)
        self.assertIn("+// verified upstream sentinel", result.stdout)


if __name__ == "__main__":
    unittest.main()
