#!/usr/bin/env python3
"""Tests of the release helpers: version bump, release notes, the GUI smoke's frame check,
the in-app update manifest, the website's release facts and publish checks."""
import json
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
import website  # noqa: E402

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
ff  Rawtonic_aarch64.app.tar.gz
"""


class Notes(unittest.TestCase):
    def test_notes_from_changelog_and_previous_notes(self):
        text = notes.notes("0.2.0-beta.2", "0.2.0-beta.1", CHANGELOG, PREV_BODY, SUMS)
        self.assertTrue(text.startswith("**Rawtonic 0.2.0-beta.2**"))
        self.assertIn("> [!WARNING]\n> This is an **early alpha**.", text)
        self.assertIn("A small update. Faster things.", text)
        self.assertIn("Updating from beta.1:", text)
        self.assertIn("edit settings stay as they are; whether saved edits render as before is listed under "
                      "Changes to existing edits below.", text)
        self.assertNotIn("render differently", text)
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
        # The updater package is a .tar.gz too, but not a camera library's source.
        self.assertIn("`libtool-2.6.2.tar.xz`", text)

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
        self.assertIn("### Windows (unsigned)", text)
        self.assertNotIn("experimental", text)
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

# alpha.18 as published: its SHA256SUMS.txt, the asset sizes GitHub reports,
# and the release.json facts the website carried (filled by hand then).
A18 = "0.1.0-alpha.18"
A18_SUMS = """db69717018fcacc5bb5cf02de8ab14faaf31771d06f531c54296d62c3ba4d6d6  Rawtonic_0.1.0-alpha.18_aarch64.app.tar.gz
110f66e25c43a41567a27fd9961724468a8ba72c41e1e3415b4f7703c7163656  Rawtonic_0.1.0-alpha.18_aarch64.dmg
094cd6c16504d49289c625d9bff4ec4b026e4df2ecf33de782343b4328866633  Rawtonic_0.1.0-alpha.18_amd64.AppImage
7bc62e26fbbf881a6c436a5ce8412176992e014077b3ad3563a7b9ac19b31602  Rawtonic_0.1.0-alpha.18_amd64.AppImage.zsync
8e64dee150cd5ce4a05c1373661ed16ea188fee831b0ca0a07fddf23eece5243  Rawtonic_0.1.0-alpha.18_x64-portable.zip
a9ff4c8a1363dad2edef99429b9606aafc2d339e3e82b61a3c6353fb11eea5f5  Rawtonic_0.1.0-alpha.18_x64-setup.exe
5b713f3d9ca3cb7185853e169b1675ece1d3d094259d6a3eef02735cece7c8be  Rawtonic_0.1.0-alpha.18_x64.msi
ff051c1723ce13ddea2176a4fbafb13ef79008b07cb77be2961bdcdb2b9213a9  Rawtonic_0.1.0-alpha.18_x86_64.app.tar.gz
6a9c091b5e63792d96368cbd77be8a7a14dfac300a739b0eb16093a8a445fd68  Rawtonic_0.1.0-alpha.18_x86_64.dmg
70fdf38898af831c69b25220d6031ba4780832c5b97f382f40c223184272dddc  SHA256SUMS-windows.txt
31a07c75cdeb7ddddf4efdd8570cf040e7a20fcd84ceede25d28a388e4721661  latest.json
5df9e774eb4ab6087a193afff84dcafe9f666e3443fe29e73e6779f659ff7f28  libgphoto2-2.5.34.tar.bz2
2ef1067c16c97db930fd740cc9bc3d3ba9a583804ae5ac42cc3e8719e49e191e  libtool-2.6.2.tar.xz
fea36f34f9156400209595e300840767ab1a385ede1dc7ee893015aea9c6dbaf  libusb-1.0.30.tar.bz2
"""
A18_SIZES = {
    "latest.json": 2819,
    "libgphoto2-2.5.34.tar.bz2": 8802205,
    "libtool-2.6.2.tar.xz": 1093564,
    "libusb-1.0.30.tar.bz2": 656112,
    "Rawtonic_0.1.0-alpha.18_aarch64.app.tar.gz": 43818716,
    "Rawtonic_0.1.0-alpha.18_aarch64.dmg": 43838391,
    "Rawtonic_0.1.0-alpha.18_amd64.AppImage": 139860472,
    "Rawtonic_0.1.0-alpha.18_amd64.AppImage.zsync": 239270,
    "Rawtonic_0.1.0-alpha.18_x64-portable.zip": 60111169,
    "Rawtonic_0.1.0-alpha.18_x64-setup.exe": 40931322,
    "Rawtonic_0.1.0-alpha.18_x64.msi": 58628773,
    "Rawtonic_0.1.0-alpha.18_x86_64.app.tar.gz": 50978829,
    "Rawtonic_0.1.0-alpha.18_x86_64.dmg": 50891141,
    "SHA256SUMS-windows.txt": 309,
}
A18_SITE = {
    "mac": ("Rawtonic_0.1.0-alpha.18_aarch64.dmg", "42 MiB", "110f66e25c43a41567a27fd9961724468a8ba72c41e1e3415b4f7703c7163656"),
    "macIntel": ("Rawtonic_0.1.0-alpha.18_x86_64.dmg", "49 MiB", "6a9c091b5e63792d96368cbd77be8a7a14dfac300a739b0eb16093a8a445fd68"),
    "linux": ("Rawtonic_0.1.0-alpha.18_amd64.AppImage", "133 MiB", "094cd6c16504d49289c625d9bff4ec4b026e4df2ecf33de782343b4328866633"),
    "windows": ("Rawtonic_0.1.0-alpha.18_x64.msi", "56 MiB", "5b713f3d9ca3cb7185853e169b1675ece1d3d094259d6a3eef02735cece7c8be"),
}
A18_OTHER = {
    "libgphoto2-2.5.34.tar.bz2": "8 MiB", "libtool-2.6.2.tar.xz": "1 MiB", "libusb-1.0.30.tar.bz2": "1 MiB",
    "Rawtonic_0.1.0-alpha.18_amd64.AppImage.zsync": "0 MiB", "Rawtonic_0.1.0-alpha.18_x64-portable.zip": "57 MiB",
    "Rawtonic_0.1.0-alpha.18_x64-setup.exe": "39 MiB", "Rawtonic_0.1.0-alpha.18_aarch64.app.tar.gz": "42 MiB",
    "Rawtonic_0.1.0-alpha.18_x86_64.app.tar.gz": "49 MiB", "latest.json": "0 MiB", "SHA256SUMS-windows.txt": "0 MiB",
    "SHA256SUMS.txt": "0 MiB",
}
A18_CHANGELOG = "# Changelog\n\n## Unreleased\n\n## 0.1.0-alpha.18 — 2026-11-19\n\nText.\n\n## 0.1.0-alpha.17 — 2026-11-17\n"


def a18_assets(d):
    """The alpha.18 assets as sparse files of their real sizes, with the real sums file."""
    for name, size in A18_SIZES.items():
        with open(Path(d, name), "wb") as f:
            f.truncate(size)
    Path(d, "SHA256SUMS.txt").write_text(A18_SUMS)


class Website(unittest.TestCase):
    def test_facts_match_what_the_site_carried_for_alpha_18(self):
        with tempfile.TemporaryDirectory() as d:
            a18_assets(d)
            f = website.facts(A18, A18_CHANGELOG, d)
        self.assertEqual((f["version"], f["date"], f["dateText"], f["tag"]),
                         (A18, "2026-11-19", "19 November 2026", "v0.1.0-alpha.18"))
        for key, (name, size, sha) in A18_SITE.items():
            self.assertEqual(f[key], {"file": name, "size": size, "sha256": sha}, key)
        self.assertEqual({o["file"]: o["size"] for o in f["otherAssets"]}, A18_OTHER)
        self.assertEqual(f["otherAssets"][-1], {"file": "SHA256SUMS.txt", "size": "0 MiB",
                         "sha256": "4f0ac247e2fca9f3c54c2b44fa6590f1737aef3cda5d9ef8a1580771bc58320c"})
        sums = dict(line.split("  ")[::-1] for line in A18_SUMS.splitlines())
        for o in f["otherAssets"][:-1]:
            self.assertEqual(o["sha256"], sums[o["file"]])

    def test_mib_rounds_half_up(self):
        mib = 1024 * 1024
        self.assertEqual([website.mib(n) for n in (0, mib // 2 - 1, mib // 2, 43838391, 139860472)],
                         ["0 MiB", "0 MiB", "1 MiB", "42 MiB", "133 MiB"])

    def test_write_keeps_whats_new_and_drops_the_old_name(self):
        with tempfile.TemporaryDirectory() as d:
            a18_assets(d)
            Path(d, "CHANGELOG.md").write_text(A18_CHANGELOG)
            rj = Path(d, "release.json")
            whats_new = {"version": A18, "en": {"summary": "s"}, "de": {"summary": "z"}}
            rj.write_text(json.dumps({"_comment": "c", "version": "0.1.0-alpha.17", "name": "Old title",
                                      "date": "2026-11-17", "whatsNew": whats_new}))
            argv = ["write", "--version", A18, "--changelog", str(Path(d, "CHANGELOG.md")), "--assets", d,
                    "--release-json", str(rj)]
            self.assertEqual(website.main(argv), 0)
            out = json.loads(rj.read_text())
            self.assertEqual(list(out)[:3], ["_comment", "version", "date"])
            self.assertNotIn("name", out)
            self.assertEqual(out["whatsNew"], whats_new)
            self.assertEqual(out["windows"]["file"], "Rawtonic_0.1.0-alpha.18_x64.msi")
            # A What's new of the previous release: the facts are written, the stage fails.
            out["whatsNew"]["version"] = "0.1.0-alpha.17"
            rj.write_text(json.dumps(out))
            self.assertEqual(website.main(argv), 1)
            self.assertEqual(json.loads(rj.read_text())["version"], A18)
            del out["whatsNew"]["de"]
            out["whatsNew"]["version"] = A18
            rj.write_text(json.dumps(out))
            self.assertEqual(website.main(argv), 1)

    def test_facts_refuse_a_missing_download_or_heading(self):
        with tempfile.TemporaryDirectory() as d:
            a18_assets(d)
            with self.assertRaisesRegex(ValueError, "no '## 0.1.0-alpha.18 — YYYY-MM-DD'"):
                website.facts(A18, "## 0.1.0-alpha.18 — soon\n", d)
            Path(d, "Rawtonic_0.1.0-alpha.18_x64.msi").unlink()
            with self.assertRaisesRegex(ValueError, "x64.msi, which is not in"):
                website.facts(A18, A18_CHANGELOG, d)
            Path(d, "SHA256SUMS.txt").write_text("".join(l + "\n" for l in A18_SUMS.splitlines() if "x64.msi" not in l))
            with self.assertRaisesRegex(ValueError, "has no Rawtonic_0.1.0-alpha.18_x64.msi"):
                website.facts(A18, A18_CHANGELOG, d)

    def page(self, version, heading):
        links = "".join(f'<a href="https://github.com/rawtonic-app/rawtonic/releases/download/v{version}/{n}">'
                        for n in website.main_files(version).values())
        return (f'<p>Rawtonic {version} · …</p><h2 class="head" id="new-title">{heading}</h2>{links}')

    def test_page_shows_the_version_in_facts_and_heading(self):
        self.assertEqual(website.page_problems(self.page(A18, "New in alpha.18."), A18, "en"), [])
        self.assertEqual(website.page_problems(self.page(A18, "Neu in alpha.18."), A18, "de"), [])
        # The live page still said "New in alpha.17.1." after alpha.18 was deployed (2026-11-19).
        stale = website.page_problems(self.page(A18, "New in alpha.17.1."), A18, "en")
        self.assertEqual(len(stale), 1)
        self.assertIn("'New in alpha.17.1.'", stale[0])
        # alpha.18 is not alpha.1 (no prefix match), and old download links are found.
        old = website.page_problems(self.page("0.1.0-alpha.17", "New in alpha.18."), A18, "en")
        self.assertEqual(sum("/download/v0.1.0-alpha.17/" in p for p in old), 4, old)
        self.assertEqual(sum("no download link" in p for p in old), 4, old)
        self.assertTrue(website.page_problems(self.page(A18, "New in alpha.18."), "0.1.0-alpha.1", "en"))
        self.assertIn("no What's new heading", website.page_problems("<p>Rawtonic</p>", A18, "en")[0])

    def test_page_links_must_all_lead_to_this_release(self):
        page = self.page(A18, "New in alpha.18.")
        # A visible button left at the previous release fails, even with V's links elsewhere.
        old = '<a class="btn" href="https://github.com/rawtonic-app/rawtonic/releases/download/v0.1.0-alpha.17/Rawtonic_0.1.0-alpha.17_aarch64.dmg">'
        problems = website.page_problems(page + old, A18, "en")
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("leads to https://github.com/rawtonic-app/rawtonic/releases/download/v0.1.0-alpha.17/", problems[0])
        # The clickable href counts, not a data-href beside it; another repository's file is not this release's.
        url = f"https://github.com/rawtonic-app/rawtonic/releases/download/v{A18}/Rawtonic_{A18}_aarch64.dmg"
        stale_href = page + f'<a href="{old[old.index("https"):-2]}" data-href="{url}">'
        self.assertEqual(len(website.page_problems(stale_href, A18, "en")), 1)
        fork = page.replace("github.com/rawtonic-app/rawtonic/", "github.com/someone/rawtonic/", 1)
        self.assertEqual(len(website.page_problems(fork, A18, "en")), 2)
        # V's links only in a comment or a script do not count.
        hidden = website.page_problems(f"<!-- {page} -->", A18, "en") + website.page_problems(f"<script>{page}</script>", A18, "en")
        self.assertEqual(sum("no download link" in p for p in hidden), 8)

    def test_whats_new_check_for_another_version(self):
        rel = {"version": "0.1.0-alpha.17", "whatsNew": {"version": A18, "en": {}, "de": {}}}
        self.assertEqual(website.whats_new_problems(rel, A18), [])
        self.assertTrue(website.whats_new_problems(rel))
        self.assertTrue(website.whats_new_problems({"version": A18}, A18))

    def test_draft_files_must_be_the_tested_ones(self):
        with tempfile.TemporaryDirectory() as d:
            a18_assets(d)
            sums = website.sums_of(d)
        listing = [{"name": n, "digest": f"sha256:{h}"} for n, h in sums.items()]
        self.assertEqual(website.assets_problems(listing, sums), [])
        # An installer replaced during review, a file added, a file missing.
        changed = [dict(a, digest="sha256:" + "0" * 64) if a["name"].endswith("x64.msi") else a for a in listing]
        self.assertIn("x64.msi on GitHub is sha256:000", website.assets_problems(changed, sums)[0])
        self.assertIn("not tested", website.assets_problems(listing + [{"name": "extra.zip", "digest": "sha256:1"}], sums)[0])
        self.assertIn("lacks latest.json", website.assets_problems([a for a in listing if a["name"] != "latest.json"], sums)[0])
        self.assertIn("without a digest", website.assets_problems([dict(a, digest=None) for a in listing], sums)[0])

    def test_manifest(self):
        self.assertEqual(website.manifest_problems(json.dumps({"version": A18, "platforms": {"x": {}}}), A18), [])
        self.assertTrue(website.manifest_problems(json.dumps({"version": "0.1.0-alpha.17", "platforms": {"x": {}}}), A18))
        self.assertTrue(website.manifest_problems("<html>Not Found</html>", A18))


if __name__ == "__main__":
    unittest.main()
