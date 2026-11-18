#!/usr/bin/env python3
"""Tests of the release helpers: version bump, release notes, the GUI smoke's frame check,
the in-app update manifest."""
import struct
import sys
import tempfile
import unittest
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bump  # noqa: E402
import gui_smoke  # noqa: E402
import notes  # noqa: E402
import updater_manifest  # noqa: E402

CHANGELOG = """# Changelog

## Unreleased

## 0.2.0-beta.2 — 2026-11-02

A small update. Faster things.

### Highlights

- **Faster.** Measured.

### Fixes

- **Fixed.** A bug.

### Changes to existing edits

- None of note.

## 0.2.0-beta.1 — 2026-10-01

Old.
"""

PREV_BODY = """**Rawtonic 0.2.0-beta.1**: the first beta.

> [!WARNING]
> This is an **early alpha**.

Old intro.

## Install

1. Download `Rawtonic_0.2.0-beta.1_aarch64.dmg` below.

## What's new since beta.0

- old

## Known limits

- old

## Source code of the camera libraries

Tarballs attached below (`libgphoto2-2.5.33.tar.bz2`, `libtool-2.6.2.tar.xz`).

## Feedback

Report problems.

## Feedback

Report problems.

---

SHA-256 (also in `SHA256SUMS.txt`):
- `Rawtonic_0.2.0-beta.1_aarch64.dmg`: `00`
"""

SUMS = """aa  Rawtonic_0.2.0-beta.2_aarch64.dmg
bb  Rawtonic_0.2.0-beta.2_x64-setup.exe
cc  SHA256SUMS-windows.txt
dd  libgphoto2-2.5.34.tar.bz2
ee  libtool-2.6.2.tar.xz
"""


class Notes(unittest.TestCase):
    def test_notes_from_changelog_and_previous_notes(self):
        text = notes.notes("0.2.0-beta.2", "0.2.0-beta.1", CHANGELOG, PREV_BODY, SUMS)
        self.assertTrue(text.startswith("**Rawtonic 0.2.0-beta.2**"))
        self.assertIn("> [!WARNING]\n> This is an **early alpha**.", text)
        self.assertIn("A small update. Faster things.", text)
        self.assertIn("Updating from beta.1:", text)
        self.assertIn("`Rawtonic_0.2.0-beta.2_aarch64.dmg` below", text)
        self.assertIn("## What's new since beta.1\n\n- **Faster.** Measured.", text)
        self.assertIn("## Fixes\n\n- **Fixed.** A bug.\n\n## Changes to existing edits\n\n- None of note.", text)
        self.assertIn("The known limits of beta.1 remain", text)
        self.assertIn("`libgphoto2-2.5.34.tar.bz2`", text)
        self.assertNotIn("Old intro", text)
        self.assertEqual(text.count("## Feedback"), 1)
        self.assertIn("- `Rawtonic_0.2.0-beta.2_x64-setup.exe`: `bb`", text)
        self.assertIn("- `SHA256SUMS-windows.txt`: `cc`", text)
        self.assertNotIn("libtool-2.6.2.tar.xz`: ", text)

    def test_a_longer_version_is_not_the_previous_one(self):
        body = PREV_BODY.replace("0.2.0-beta.1", "0.2.0")
        text = notes.notes("0.2.0.1", "0.2.0", CHANGELOG.replace("0.2.0-beta.2", "0.2.0.1"), body,
                           SUMS.replace("0.2.0-beta.2", "0.2.0.1"))
        self.assertIn("`Rawtonic_0.2.0.1_aarch64.dmg` below", text)

    def test_windows_install_names_the_msi_and_the_zip(self):
        body = PREV_BODY.replace("## What's new since beta.0",
                                 "### Windows (experimental, unsigned)\n\n1. Download the old installer.\n\n"
                                 "## What's new since beta.0")
        sums = SUMS + "ff  Rawtonic_0.2.0-beta.2_x64.msi\n11  Rawtonic_0.2.0-beta.2_x64-portable.zip\n"
        text = notes.notes("0.2.0-beta.2", "0.2.0-beta.1", CHANGELOG, body, sums)
        self.assertIn("- `Rawtonic_0.2.0-beta.2_x64.msi`: **the recommended download.**", text)
        self.assertIn("`Rawtonic_0.2.0-beta.2_x64-portable.zip`: portable", text)
        self.assertIn("start `Rawtonic\\rawtonic-app.exe`", text)
        self.assertNotIn("the old installer", text)
        self.assertIn("`Rawtonic_0.2.0-beta.2_aarch64.dmg` below", text)
        self.assertIn("- `Rawtonic_0.2.0-beta.2_x64.msi`: `ff`", text)
        self.assertIn("- `Rawtonic_0.2.0-beta.2_x64-portable.zip`: `11`", text)
        # Without the MSI the previous text stays.
        self.assertIn("the old installer", notes.notes("0.2.0-beta.2", "0.2.0-beta.1", CHANGELOG, body, SUMS))

    def test_missing_section_fails(self):
        with self.assertRaisesRegex(ValueError, "no '## 0.3.0"):
            notes.notes("0.3.0", "0.2.0-beta.1", CHANGELOG, PREV_BODY, SUMS)


class Bump(unittest.TestCase):
    def tree(self, unreleased="Text.\n\n### Highlights\n\n- x\n"):
        root = Path(tempfile.mkdtemp())
        (root / "src-tauri/resources/licenses").mkdir(parents=True)
        (root / "package.json").write_text('{\n  "name": "rawtonic",\n  "version": "0.1.0",\n  "dependencies": {"a": "1"}\n}\n')
        (root / "package-lock.json").write_text(
            '{\n  "version": "0.1.0",\n  "packages": {\n    "": {\n      "version": "0.1.0"\n    },\n'
            '    "node_modules/x": {\n      "version": "0.1.0"\n    }\n  }\n}\n')
        (root / "src-tauri/tauri.conf.json").write_text('{\n  "version": "0.1.0"\n}\n')
        (root / "src-tauri/Cargo.toml").write_text(
            '[package]\nname = "rawtonic-app"\nversion = "0.1.0"\n\n[dependencies]\nx = { version = "0.1.0" }\n')
        for rel in ("THIRD_PARTY_NOTICES.md", "src-tauri/resources/licenses/THIRD_PARTY_NOTICES.md"):
            (root / rel).write_text("| rawtonic-app | 0.1.0 | AGPL-3.0-or-later |\n| rawtonic-cli | 0.1.0 | AGPL |\n")
        (root / "CHANGELOG.md").write_text(f"# Changelog\n\n## Unreleased\n\n{unreleased}\n## 0.1.0 — 2026-10-01\n\nOld.\n")
        return root

    def test_every_spot_and_the_changelog_heading(self):
        root = self.tree()
        edits = bump.bump(root, "0.2.0", "2026-11-02")["edits"]
        self.assertIn('"version": "0.2.0"', edits["package.json"])
        lock = edits["package-lock.json"]
        self.assertEqual(lock.count('"version": "0.2.0"'), 2)
        self.assertEqual(lock.count('"version": "0.1.0"'), 1)
        self.assertIn('version = "0.2.0"', edits["src-tauri/Cargo.toml"])
        self.assertIn('x = { version = "0.1.0" }', edits["src-tauri/Cargo.toml"])
        self.assertIn("| rawtonic-app | 0.2.0 |", edits["THIRD_PARTY_NOTICES.md"])
        self.assertIn("| rawtonic-cli | 0.1.0 |", edits["THIRD_PARTY_NOTICES.md"])
        self.assertTrue(edits["CHANGELOG.md"].startswith(
            "# Changelog\n\n## Unreleased\n\n## 0.2.0 — 2026-11-02\n\nText.\n\n### Highlights\n\n- x\n\n## 0.1.0 — "))

    def test_empty_unreleased_fails(self):
        with self.assertRaisesRegex(ValueError, "empty"):
            bump.bump(self.tree(unreleased=""), "0.2.0", "2026-11-02")

    def test_same_version_fails(self):
        with self.assertRaisesRegex(ValueError, "already"):
            bump.bump(self.tree(), "0.1.0", "2026-11-02")


def png(width, height, pixel):
    raw = b"".join(b"\x00" + b"".join(bytes(pixel(x, y)) for x in range(width)) for y in range(height))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


class Frame(unittest.TestCase):
    def test_a_flat_viewer_is_blank(self):
        self.assertFalse(gui_smoke.viewer_stats(png(200, 120, lambda x, y: (40, 40, 40)))["ok"])

    def test_a_picture_is_not_blank(self):
        stats = gui_smoke.viewer_stats(png(200, 120, lambda x, y: ((x * 7) % 256, (y * 5) % 256, (x * y) % 256)))
        self.assertTrue(stats["ok"], stats)


# A throwaway test key's public half and its signature (`tauri signer sign
# --app-version 0.2.0`) of PACKAGE; the private half was never kept.
TEST_PUBLIC = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQwRUEwNzIxNzI2NzQ5MzYKUldRMlNXZHlJUWZxMEtSUUNjSHlyR1FaK0NRekVoYytlZXFucjdNTmVxR1BxOEZrUlZaSjhXeFcK"
PACKAGE = b"Rawtonic update package test bytes\n"
PACKAGE_SIG = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRMlNXZHlJUWZxMEUrOEUrb3UreTNiYVBKR1loNnpuSHQ2K0VvdzhyUFZ5UEttZHB6UHppUFdGN0tqQ0xKRXA4YzRBVVpRMXROZXVFK3R0aDR3dEo4M3k0Z210R1lROXc0PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk1MDM2ODM1CWZpbGU6cGtnLmJpbgl2ZXJzaW9uOjAuMi4wCjA1cmU1MnhqWTVBaHZYaS9kY3pJSzQ3WEZjcTVRdFhJRUx5QXpNRlMrcGU3Ung3bXNka0VSR1RjblVKTU5qTTNURjJXalRtczl2cXlPb0t4SG9NN0JBPT0K"
BASE = "https://github.com/rawtonic-app/rawtonic/releases/download/v0.2.0/"


class UpdaterManifest(unittest.TestCase):
    def test_rfc8032_vector(self):
        public = bytes.fromhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
        signature = bytes.fromhex(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        )
        self.assertTrue(updater_manifest.ed25519_verify(public, b"", signature))
        self.assertFalse(updater_manifest.ed25519_verify(public, b"x", signature))

    def test_tauri_signature_checks(self):
        check = updater_manifest.check_signature
        self.assertIsNone(check(PACKAGE, PACKAGE_SIG, TEST_PUBLIC, "0.2.0"))
        self.assertIn("does not match", check(PACKAGE + b"x", PACKAGE_SIG, TEST_PUBLIC, "0.2.0"))
        self.assertIn("version", check(PACKAGE, PACKAGE_SIG, TEST_PUBLIC, "0.2.1"))
        other = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEE0MzNCOEI1RjE5MkExRTAKUldUZ29aTHh0Ymd6cExLbEdCU2thUmJzQkRtSEhNeEZTdFlPNVRuZ291YkdXSnFWaHQ4OFoya3cK"
        self.assertIn("another key", check(PACKAGE, PACKAGE_SIG, other, "0.2.0"))

    def run_tool(self, *argv):
        old = sys.argv
        sys.argv = ["updater_manifest.py", *argv]
        try:
            updater_manifest.main()
        finally:
            sys.argv = old

    def test_make_then_verify_against_the_release_files(self):
        with tempfile.TemporaryDirectory() as d:
            Path(d, "pkg.bin").write_bytes(PACKAGE)
            Path(d, "pkg.bin.sig").write_text(PACKAGE_SIG)
            manifest = str(Path(d, "latest.json"))
            self.run_tool("make", "--version", "0.2.0", "--base-url", BASE, "--pub-date", "2026-11-18T00:00:00Z",
                          "--notes", "n", "--sig-dir", d, "--out", manifest, "linux-x86_64-appimage=pkg.bin")
            verify = ["verify", "--manifest", manifest, "--pubkey", TEST_PUBLIC, "--version", "0.2.0",
                      "--base-url", BASE, "--dir", d]
            self.run_tool(*verify, "--expect", "linux-x86_64-appimage")
            Path(d, "names.txt").write_text("pkg.bin\nlatest.json\n")
            self.run_tool(*verify, "--names", str(Path(d, "names.txt")))
            for extra in (["--expect", "windows-x86_64-nsis"], ["--names", str(Path(d, "pkg.bin.sig"))]):
                with self.assertRaises(SystemExit):
                    self.run_tool(*verify, *extra)
            with self.assertRaises(SystemExit):
                self.run_tool(*verify[:-2], "--base-url", BASE.replace("v0.2.0", "v0.1.0"), "--dir", d)


if __name__ == "__main__":
    unittest.main()
