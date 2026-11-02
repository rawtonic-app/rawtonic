#!/usr/bin/env python3
"""Tests of the release helpers: version bump, release notes, the GUI smoke's frame check."""
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

CHANGELOG = """# Changelog

## Unreleased

## 0.2.0-beta.2 — 2026-11-02

A small update. Faster things.

### Highlights

- **Faster.** Measured.

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
        self.assertIn("## Changes to existing edits\n\n- None of note.", text)
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


if __name__ == "__main__":
    unittest.main()
