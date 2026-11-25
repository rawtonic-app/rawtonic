#!/usr/bin/env python3
"""macOS file associations: Rawtonic is offered for photos but takes over no default.

LaunchServices picks a file's default app by how exactly an app's
CFBundleDocumentTypes entry matches the file, before LSHandlerRank. Measured
with stub bundles registered one at a time on macOS 27 (notes:
benchmark-results/uti1-2026-11-25): an entry naming a vendor RAW type
(com.nikon.raw-image) or listing only the extension becomes the default over
Preview, which claims public.camera-raw-image, at any rank. An entry claiming
public.camera-raw-image at Alternate rank is listed and leaves Preview (or the
user's choice) the default; Alternate also loses a tie on an exact type
(public.png) or extension (x3f) against another app's Default.

So on macOS (tauri.macos.conf.json replaces the shared list, which Windows and
Linux keep for their per-extension associations) Rawtonic claims RAW files
through the parent type only and every entry is Alternate. Rawtonic declares no
file type of its own; one it adds may use Owner with an exported type.
"""

import json
from pathlib import Path
import unittest

REPO = Path(__file__).resolve().parent.parent

# The vendor RAW extensions macOS itself declares as conforming to
# public.camera-raw-image (CoreTypes, macOS 13 to 27). Claiming them by type or
# by extension makes Rawtonic the default, so they go through the parent type.
SYSTEM_RAW_EXTENSIONS = {
    "dng", "crw", "cr2", "cr3", "raw", "erf", "raf", "3fr", "fff", "iiq", "dcr",
    "mos", "rwl", "mrw", "nef", "nrw", "orf", "ori", "rw2", "pef", "srw", "arw",
    "srf", "sr2", "pwl", "nefx", "axr", "dxo",
}
CAMERA_RAW = "public.camera-raw-image"


def associations(name):
    return json.loads((REPO / "src-tauri" / name).read_text())["bundle"]["fileAssociations"]


class MacFileTypesTests(unittest.TestCase):
    def setUp(self):
        self.mac = associations("tauri.macos.conf.json")
        self.shared = associations("tauri.conf.json")

    def test_every_shared_extension_stays_associated_on_macos(self):
        shared = {e for a in self.shared for e in a["ext"]}
        mac = {e for a in self.mac for e in a["ext"]}
        self.assertEqual(shared - mac, set())

    def test_no_entry_takes_a_default(self):
        for a in self.mac:
            self.assertEqual(a.get("rank"), "Alternate", a["name"])
            self.assertNotIn("exportedType", a, a["name"])

    def test_system_raw_types_are_claimed_only_through_the_parent_type(self):
        raw = [a for a in self.mac if CAMERA_RAW in a.get("contentTypes", [])]
        self.assertEqual(len(raw), 1)
        self.assertEqual(raw[0]["contentTypes"], [CAMERA_RAW])
        for a in self.mac:
            for t in a.get("contentTypes", []):
                self.assertFalse(t.endswith("raw-image") and t != CAMERA_RAW, t)
            if a is not raw[0]:
                self.assertEqual(SYSTEM_RAW_EXTENSIONS & set(a["ext"]), set(), a["name"])

    def test_extension_only_entries_name_no_type_the_bundler_would_fill_in(self):
        # Tauri fills LSItemContentTypes for well-known extensions, and macOS
        # then ignores the entry's extension list; known types get their own entry.
        known = {"jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "exr", "webp", "heic", "heif"}
        for a in self.mac:
            if "contentTypes" not in a:
                self.assertEqual(known & set(a["ext"]), set(), a["name"])


if __name__ == "__main__":
    unittest.main()
