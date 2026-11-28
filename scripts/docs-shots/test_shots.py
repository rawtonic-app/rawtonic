#!/usr/bin/env python3
"""The documentation shot list (scripts/docs-shots/shots.json) still fits the app:
every step is one capture.py knows, every i18n key exists in each interface
language, every tool title and tab is one Develop has. A renamed string or tool
fails here, in scripts/check.sh, instead of at the next release's screenshots.

    python3 scripts/docs-shots/test_shots.py
"""
import json
import re
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(HERE))
import capture  # noqa: E402

LANGS = ("en", "de")


def steps(shot):
    yield from shot.get("steps", [])
    for part in shot.get("crop", {}).get("union", []):
        yield {"op": "crop", **part}


class ShotList(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.spec = json.loads((HERE / "shots.json").read_text())
        cls.strings = {lang: json.loads((REPO / f"src/i18n/locales/{lang}.json").read_text()) for lang in LANGS}
        source = (HERE / "capture.py").read_text()
        cls.ops = set(re.findall(r'op == "([a-z_]+)"', source)) | {"crop"}
        studio = "".join(p.read_text() for p in (REPO / "src/components/studio").glob("*.tsx"))
        cls.tool_titles = set(re.findall(r'<ToolGroup[^>]*?\btitle="([^"]+)"', studio, re.S))

    def test_ids_are_unique_and_plain(self):
        ids = [s["id"] for s in self.spec["shots"]]
        self.assertEqual(len(ids), len(set(ids)))
        for i in ids:
            self.assertRegex(i, r"^[a-z0-9-]+$")

    def test_every_step_is_known(self):
        for shot in self.spec["shots"]:
            for step in steps(shot):
                self.assertIn(step["op"], self.ops, f"{shot['id']}: unknown step {step['op']}")

    def test_every_string_exists_in_each_language(self):
        for shot in self.spec["shots"]:
            for step in steps(shot):
                keys = [step[k] for k in ("name_key", "text_key") if k in step] + list(step.get("var_keys", {}).values())
                for key in keys:
                    for lang in LANGS:
                        with self.subTest(shot=shot["id"], key=key, lang=lang):
                            capture.lookup(self.strings[lang], key)

    def test_tools_and_tabs_exist(self):
        for shot in self.spec["shots"]:
            for step in steps(shot):
                if step["op"] == "tool":
                    self.assertIn(step["tool"], self.tool_titles, f"{shot['id']}: no ToolGroup titled {step['tool']}")
                if step["op"] == "tab":
                    self.assertIn(step["tab"], capture.DEVELOP_TABS)

    def test_crops_lie_in_the_window(self):
        w, h = self.spec["window"]
        for shot in self.spec["shots"]:
            rect = shot.get("crop", {}).get("rect")
            if rect:
                x, y, cw, ch = rect
                self.assertTrue(0 <= x and 0 <= y and x + cw <= w and y + ch <= h, shot["id"])


if __name__ == "__main__":
    unittest.main(verbosity=1)
