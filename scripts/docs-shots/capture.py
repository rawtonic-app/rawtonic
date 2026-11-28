#!/usr/bin/env python3
"""Screenshots for the user documentation on rawtonic.cc, made by the app.

    scripts/docs-shots/capture.py --app "Rawtonic QA.app" --cli target/release/rawtonic \
        --demo DEMO --out DIR [--shots scripts/docs-shots/shots.json] [--places FILE] \
        [--lang en,de] [--only ID,...] [--models DIR] [--port 7988] [--version V]

A QA build (qa-control feature, VITE_RAWTONIC_QA=1, a QA identifier, never the
user's cc.rawtonic.editor) opens a copy of the demo library and is driven
through the QA control channel (docs/testing/qa-control.md) through each shot
of the shot list: open a photo, open a tool, press keys, click, wait; then the
app captures its own window at Retina 2x (`window.capture` best) and the shot
is cropped to its element and written as DIR/<lang>/<id>.png.

DEMO is a private folder, never committed (photographs and their licences:
the website's CREDITS.md):
  DEMO/session/   a Rawtonic session with the photos and a few edits
  DEMO/card/      optional: a memory card's folder to import from (step answer)
  DEMO/config/    optional files copied into the app's config folder of the
                  fresh profile (settings.json, presets/presets.json)
Everything runs in a fresh HOME under DIR/run-<time> on a copy of the session
(APFS clone): the demo library, the user's profile and the user's photos are
never touched. --models DIR clones an installed model folder into the fresh
profile so the AI tools need no download.

--places FILE lists the screenshot places the website has (one id per line).
Places without a shot in the list and shots that failed are reported; they
never fail the run (exit 1 only when the app could not be run at all).
DIR/result.json holds every shot and its result.
"""
import argparse
import json
import os
import plistlib
import shutil
import socket
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "scripts"))
from qa_control import QaControl  # noqa: E402

USER_IDENTIFIER = "cc.rawtonic.editor"
DEVELOP_TABS = ["adjust", "color", "detail", "crop", "retouch", "layers"]


class ShotFailed(RuntimeError):
    pass


def lookup(tree, key):
    node = tree
    for part in key.split("."):
        if not isinstance(node, dict) or part not in node:
            raise ShotFailed(f"no string {key}")
        node = node[part]
    if not isinstance(node, str):
        raise ShotFailed(f"{key} is not a string")
    return node


def self_contained(session):
    """The problems that would let a clone of SESSION reach folders outside it: an
    absolute or ../ Capture/Output in session.json, a symbolic link, a catalog root
    elsewhere (a Capture on another drive), a hot folder or an unfinished import or
    task the app would resume. The app would open those live folders, not the clone."""
    problems = []
    for folder, dirs, files in os.walk(session):
        for name in dirs + files:
            if os.path.islink(os.path.join(folder, name)):
                problems.append(f"symbolic link {os.path.relpath(os.path.join(folder, name), session)}")
    info = json.loads((session / "session.json").read_text())
    for key in ("capture", "output", "selects", "trash"):
        value = info.get(key)
        if isinstance(value, str) and (os.path.isabs(value) or ".." in Path(value).parts):
            problems.append(f"session.json {key} = {value}")
    db = session / "session.sqlite"
    if db.is_file():
        con = sqlite3.connect(f"file:{db}?immutable=1", uri=True)
        try:
            tables = {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type = 'table'")}
            for path, external in con.execute("SELECT path, external FROM roots"):
                if external or os.path.isabs(path) or ".." in Path(path).parts:
                    problems.append(f"catalog root {path}")
            if "hot_folder" in tables:
                problems += [f"hot folder {f}" for (f,) in con.execute("SELECT folder FROM hot_folder")]
            if "ingest_items" in tables:
                problems += [f"unfinished import of {src}" for (src,) in con.execute(
                    "SELECT source FROM ingest_items WHERE state IN ('pending', 'copying')")]
            if "tasks" in tables:
                problems += [f"unfinished task {kind} ({state})" for kind, state in con.execute(
                    "SELECT kind, state FROM tasks WHERE state NOT IN ('done', 'failed', 'cancelled')")]
        finally:
            con.close()
    return problems


def clone_tree(src, dst):
    """APFS clone (no extra disk space) with a plain copy as the fallback."""
    if subprocess.run(["cp", "-c", "-R", str(src), str(dst)], capture_output=True).returncode != 0:
        shutil.copytree(src, dst, symlinks=True)


class Capture:
    def __init__(self, args):
        self.args = args
        self.app = Path(args.app)
        info = plistlib.loads((self.app / "Contents/Info.plist").read_bytes())
        self.identifier = info["CFBundleIdentifier"]
        if self.identifier == USER_IDENTIFIER or not self.identifier.startswith("cc.rawtonic.qa"):
            raise SystemExit(f"docs shots: {self.identifier} is not a QA identifier (cc.rawtonic.qa…)")
        self.binary = self.app / "Contents/MacOS" / info["CFBundleExecutable"]
        self.version = args.version or info.get("CFBundleShortVersionString", "")
        self.demo = Path(args.demo).resolve()
        if not (self.demo / "session" / "session.json").is_file():
            raise SystemExit(f"docs shots: {self.demo}/session is not a Rawtonic session")
        outside = self_contained(self.demo / "session")
        if outside:
            raise SystemExit("docs shots: the demo session reaches folders outside itself, which a clone "
                             "would still open: " + "; ".join(outside))
        self.spec = json.loads(Path(args.shots).read_text())
        # Absolute: the app reports the session by its absolute path.
        self.out = Path(args.out).resolve()
        self.run = self.out / time.strftime("run-%Y%m%d-%H%M%S")
        self.strings = {}
        self.proc = None
        self.qa = None
        self.results = []

    # --- profile and app ----------------------------------------------------
    def strings_for(self, lang):
        if lang not in self.strings:
            self.strings[lang] = json.loads((REPO / f"src/i18n/locales/{lang}.json").read_text())
        return self.strings[lang]

    def prepare(self, label, lang, window, extra_settings=None):
        """A fresh HOME with a clone of the demo session; settings for this language."""
        base = self.run / label
        home = base / "home"
        config = home / "Library/Application Support" / self.identifier
        config.mkdir(parents=True)
        # Named as the session is (the Folders panel shows the folder's name).
        name = json.loads((self.demo / "session" / "session.json").read_text()).get("name") or "Session"
        session = base / name
        clone_tree(self.demo / "session", session)
        # A memory card to import from (DEMO/card), a clone as well.
        if (self.demo / "card").is_dir():
            clone_tree(self.demo / "card", base / "card")
        demo_config = self.demo / "config"
        settings = {}
        if demo_config.is_dir():
            shutil.copytree(demo_config, config, dirs_exist_ok=True)
            if (config / "settings.json").is_file():
                settings = json.loads((config / "settings.json").read_text())
        settings.update(self.spec.get("settings", {}))
        settings.update(extra_settings or {})
        settings.update({
            "language": lang,
            "interfaceShade": settings.get("interfaceShade") or self.spec.get("shade", "darkest"),
            # No What's new over the first shot, no update check (network, a banner).
            "lastRunVersion": self.version,
            "checkForUpdates": False,
        })
        (config / "settings.json").write_text(json.dumps(settings, indent=2))
        (config / "last-library.json").write_text(json.dumps(str(session)))
        scale = 2  # the window state is in backing pixels; the capture checks the size
        (config / "window_state.json").write_text(json.dumps({
            "width": window[0] * scale, "height": window[1] * scale, "x": 40 * scale, "y": 40 * scale,
            "maximized": False, "scale_factor": scale, "fullscreen": False}))
        if self.args.models:
            clone_tree(Path(self.args.models), config / "models")
        return home, session

    def launch(self, home, log):
        with socket.socket() as s:
            if s.connect_ex(("127.0.0.1", self.args.port)) == 0:
                raise RuntimeError(f"port {self.args.port} is in use (another QA app?)")
        env = {**os.environ, "HOME": str(home), "RAWTONIC_QA_PORT": str(self.args.port)}
        self.proc = subprocess.Popen([str(self.binary)], env=env, stdout=open(log, "wb"),
                                     stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"the app exited at start (exit {self.proc.returncode}; another instance?)")
            try:
                self.qa = QaControl(self.args.port, timeout=60)
                self.qa.call("ping")
                break
            except OSError:
                time.sleep(0.5)
        else:
            raise RuntimeError("the QA channel did not answer within 90 s")
        while time.monotonic() < deadline:
            try:
                self.qa.call("ui.viewport")
                return
            except RuntimeError as error:
                if "no webview reply" not in str(error):
                    raise
                time.sleep(0.5)
        raise RuntimeError("the webview's QA bridge did not answer (not a VITE_RAWTONIC_QA=1 build?)")

    def quit(self):
        subprocess.run(["osascript", "-e", f'tell application id "{self.identifier}" to quit'],
                       capture_output=True, timeout=30, check=False)
        try:
            self.proc.wait(timeout=45)
        except subprocess.TimeoutExpired:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self.qa:
            self.qa.sock.close()
            self.qa = None

    def wait_state(self, path, equals=None, timeout_ms=60000):
        """state.wait in 25 s slices (the native side gives up on one call after 30 s)."""
        deadline = time.monotonic() + timeout_ms / 1000
        while True:
            left = int((deadline - time.monotonic()) * 1000)
            params = {"path": path, "timeout_ms": max(100, min(left, 25000))}
            if equals is not None:
                params["equals"] = equals
            try:
                return self.qa.call("state.wait", params)
            except RuntimeError:
                if time.monotonic() >= deadline:
                    raise ShotFailed(f"{path} did not become {equals!r} within {timeout_ms} ms")

    # --- steps ----------------------------------------------------------------
    def query(self, step, lang):
        q = {}
        if "selector" in step:
            q["selector"] = step["selector"]
        if "name_key" in step:
            q["name"] = lookup(self.strings_for(lang), step["name_key"])
            values = dict(step.get("vars", {}))
            values.update({var: lookup(self.strings_for(lang), key) for var, key in step.get("var_keys", {}).items()})
            for var, value in values.items():
                q["name"] = q["name"].replace("{{" + var + "}}", value)
            q["exact"] = step.get("exact", True)
        elif "name" in step:
            q["name"] = step["name"]
        if "role" in step:
            q["role"] = step["role"]
        return q

    def element(self, step, lang):
        q = self.query(step, lang)
        found = self.qa.call("ui.elements", {**q, "limit": 50})
        if not found:
            raise ShotFailed(f"no element {q}")
        return found[step.get("index", 0)]

    def open_photo(self, name, session):
        target = str(session / "Capture" / name)
        if (self.qa.call("state.get", {"path": "ui.activeView"}) == "editor"
                and self.qa.call("state.get", {"path": "editor.selectedImage.path"}) == target):
            return
        # G: the Library grid (from Develop, the loupe or the multi view).
        self.qa.call("ui.key", {"key": "g", "code": "KeyG"})
        self.wait_state("ui.activeView", "library", 20000)
        deadline = time.monotonic() + 30
        tiles = []
        tries = 0
        while time.monotonic() < deadline:
            tiles = [e for e in self.qa.call("ui.elements", {"name": name, "limit": 50}) if e["tag"] == "button"]
            if tiles:
                break
            tries += 1
            if tries % 4 == 0:
                self.qa.call("ui.key", {"key": "g", "code": "KeyG"})
            time.sleep(0.5)
        if not tiles:
            raise ShotFailed(f"no library tile for {name}")
        t = tiles[0]
        self.qa.call("input.click", {"x": t["x"] + t["width"] / 2, "y": t["y"] + t["height"] / 2, "count": 2})
        self.wait_state("ui.activeView", "editor", 20000)
        self.wait_state("editor.selectedImage.path", target, 30000)
        self.wait_state("editor.hasRenderedFirstFrame", True, 60000)

    def do(self, step, lang, session):
        op = step["op"]
        if op == "open":
            self.open_photo(step["photo"], session)
        elif op == "key":
            self.qa.call("ui.key", {"key": step.get("key", ""), "code": step["code"],
                                    "modifiers": step.get("modifiers", [])})
        elif op == "click":
            e = self.element(step, lang)
            x, y = e["x"] + e["width"] / 2, e["y"] + e["height"] / 2
            if step.get("native"):
                self.qa.call("input.click", {"x": x, "y": y, "count": step.get("count", 1),
                                             "modifiers": step.get("modifiers", []), "activate": True})
            else:
                self.qa.call("ui.click", {**self.query(step, lang), "index": step.get("index", 0),
                                          "modifiers": step.get("modifiers", [])})
        elif op == "tab":
            # The Develop tool tabs in their order (StudioDevelop.tsx, studio-tool-tabs).
            n = DEVELOP_TABS.index(step["tab"]) + 1
            self.qa.call("ui.click", {"selector": f".studio-tool-tabs > button:nth-child({n})"})
        elif op == "tool":
            # Open this tool group alone (⌥-click on its header, as a user does).
            sel = f'details[data-tool-title="{step["tool"]}"] > summary'
            self.qa.call("ui.wait", {"selector": sel, "timeout_ms": 20000})
            self.qa.call("ui.click", {"selector": sel, "modifiers": ["alt"]})
            if not self.qa.call("ui.elements", {"selector": f'details[open][data-tool-title="{step["tool"]}"]'}):
                self.qa.call("ui.click", {"selector": sel})
        elif op == "type":
            text = lookup(self.strings_for(lang), step["text_key"]) if "text_key" in step else str(step["text"])
            self.qa.call("ui.type", {**self.query(step, lang), "text": text})
            if step.get("enter"):
                self.qa.call("ui.key", {"key": "Enter", "code": "Enter"})
        elif op == "wait":
            self.qa.call("ui.wait", {**self.query(step, lang), "timeout_ms": step.get("timeout_ms", 20000)})
        elif op == "state":
            self.wait_state(step["path"], step.get("equals"), step.get("timeout_ms", 60000))
        elif op == "click_at":
            # A native click at a point of the window (on the photograph: a picker, a zoom).
            self.qa.call("input.click", {"x": step["x"], "y": step["y"], "count": step.get("count", 1),
                                         "modifiers": step.get("modifiers", []), "activate": True})
        elif op == "drag":
            self.qa.call("input.drag", {"points": step["points"], "activate": True,
                                        "interval_ms": step.get("interval_ms", 16.7)})
        elif op == "answer":
            # What the next open dialog returns (a path in this shot's folder: the card).
            self.qa.call("dialog.answer", {"path": str(session.parent / step["path"])})
        elif op == "mouse":
            # A native pointer move: hover-driven views (zoom at the pointer, the picker loupe).
            self.qa.call("input.mouse", {"type": "move", "x": step["x"], "y": step["y"]})
        elif op == "hover":
            self.qa.call("ui.hover", {"points": step["points"]})
        elif op == "sleep":
            time.sleep(step["ms"] / 1000)
        else:
            raise ShotFailed(f"unknown step {op}")

    # --- capture --------------------------------------------------------------
    def crop_box(self, crop, lang):
        """The crop in CSS pixels: the union of the named elements, padded, inside the window."""
        view = self.qa.call("ui.viewport")
        vw, vh = view["width"], view["height"]
        if not crop:
            return None, vw
        if "rect" in crop:
            x0, y0, w, h = crop["rect"]
            x1, y1 = x0 + w, y0 + h
        else:
            boxes = [self.element(part, lang) for part in crop["union"]]
            x0 = min(b["x"] for b in boxes)
            y0 = min(b["y"] for b in boxes)
            x1 = max(b["x"] + b["width"] for b in boxes)
            y1 = max(b["y"] + b["height"] for b in boxes)
        pad = crop.get("pad", 0)
        x0, y0 = max(0, x0 - pad), max(0, y0 - pad)
        x1, y1 = min(vw, x1 + pad), min(vh, y1 + pad)
        if "min_width" in crop and x1 - x0 < crop["min_width"]:
            x0 = max(0, x1 - crop["min_width"])
        return (x0, y0, x1, y1), vw

    def capture(self, shot, lang, path):
        box, vw = self.crop_box(shot.get("crop"), lang)
        result = self.qa.call("window.capture", {"resolution": "best", "color_space": "srgb"})
        import base64
        raw = path.with_suffix(".window.png")
        raw.write_bytes(base64.b64decode(result["png_base64"]))
        scale = result["width"] / vw
        if scale < 1.9:
            raw.unlink()
            raise ShotFailed(f"capture is {result['width']} px for a {vw} px window: not a Retina 2x display")
        # The new picture replaces the earlier one only once it is complete;
        # result.json says which pictures this run made.
        if box is None:
            raw.replace(path)
        else:
            x0, y0, x1, y1 = (round(v * scale) for v in box)
            part = path.with_suffix(".part.png")
            r = subprocess.run(["magick", str(raw), "-crop", f"{x1 - x0}x{y1 - y0}+{x0}+{y0}", "+repage",
                                str(part)], capture_output=True, text=True)
            raw.unlink()
            if r.returncode:
                part.unlink(missing_ok=True)
                raise ShotFailed(f"crop: {r.stderr.strip()[-200:]}")
            part.replace(path)
        return {"width": result["width"], "scale": scale}

    # --- run --------------------------------------------------------------------
    def jobs(self):
        only = set(self.args.only.split(",")) if self.args.only else None
        langs = self.args.lang.split(",")
        for shot in self.spec["shots"]:
            if only and shot["id"] not in only:
                continue
            for lang in shot.get("langs", langs):
                if lang in langs:
                    yield lang, shot

    def run_shot(self, lang, shot):
        """One shot in its own fresh profile and app run: no state of another shot
        (a view, a selection, a zoom, an edit) leaks into it."""
        window = tuple(shot.get("window", self.spec.get("window", [1440, 900])))
        label = f"{lang}-{shot['id']}"
        home, session = self.prepare(label, lang, window, shot.get("settings"))
        path = self.out / lang / f"{shot['id']}.png"
        path.parent.mkdir(parents=True, exist_ok=True)
        entry = {"id": shot["id"], "lang": lang}
        self.launch(home, self.run / f"{label}.stdout.log")
        try:
            self.wait_state("library.sessionDirectory", str(session), 60000)
            self.wait_state("library.imageList.0.path", None, 60000)
            info = self.qa.call("window.info")
            if (info.get("width"), info.get("height")) != window:
                raise ShotFailed(f"window {info.get('width')}x{info.get('height')} instead of "
                                 f"{window[0]}x{window[1]} (screen too small?)")
            if "photo" in shot:
                self.open_photo(shot["photo"], session)
            for step in shot.get("steps", []):
                self.do(step, lang, session)
            time.sleep(shot.get("settle_ms", 2500) / 1000)
            entry.update(self.capture(shot, lang, path), ok=True, file=str(path))
            print(f"ok   {lang} {shot['id']}", flush=True)
        except (ShotFailed, RuntimeError) as error:
            entry.update(ok=False, error=str(error))
            print(f"FAIL {lang} {shot['id']}: {error}", flush=True)
            try:
                self.qa.screenshot(str(self.run / f"failure-{label}.png"))
            except Exception:
                pass
        finally:
            self.results.append(entry)
            self.quit()
            # The profile holds a clone of the models and the session: gone with the shot.
            shutil.rmtree(home.parent, ignore_errors=True)

    def main(self):
        self.run.mkdir(parents=True)
        fatal = None
        started = 0
        for lang, shot in self.jobs():
            try:
                self.run_shot(lang, shot)
                started += 1
            except Exception as error:  # the app could not be run: report, go on
                fatal = f"{lang} {shot['id']}: {error}"
                print(f"FAIL {fatal}", flush=True)
                self.results.append({"id": shot["id"], "lang": lang, "ok": False, "error": str(error)})
                if self.proc and self.proc.poll() is None:
                    self.proc.kill()
        if started:  # one shot that ran is enough to call the app runnable
            fatal = None
        if self.results and not any(r["ok"] for r in self.results):
            fatal = fatal or "no shot was made (the setup, not one shot, is wrong: see the failure-*.png)"
        defined = {s["id"] for s in self.spec["shots"]}
        places = []
        if self.args.places:
            places = sorted({p.strip() for p in Path(self.args.places).read_text().split() if p.strip()})
        no_recipe = [p for p in places if p not in defined]
        failed = sorted({f"{r['lang']} {r['id']}" for r in self.results if not r["ok"]})
        summary = {"version": self.version, "ok": fatal is None, "fatal": fatal, "shots": self.results,
                   "places": places, "places_without_shot": no_recipe, "failed": failed}
        (self.out / "result.json").write_text(json.dumps(summary, indent=2))
        made = sum(1 for r in self.results if r["ok"])
        print(f"docs shots: {made} made, {len(failed)} failed{': ' + ', '.join(failed) if failed else ''}")
        if no_recipe:
            print(f"docs shots: {len(no_recipe)} place(s) on the website without a shot in "
                  f"{self.args.shots}: {', '.join(no_recipe)}")
        return 0 if fatal is None else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--app", required=True)
    parser.add_argument("--demo", required=True, help="the demo library (session/, config/)")
    parser.add_argument("--out", required=True)
    parser.add_argument("--shots", default=str(HERE / "shots.json"))
    parser.add_argument("--places", help="file with the website's screenshot place ids")
    parser.add_argument("--lang", default="en,de")
    parser.add_argument("--only", help="comma-separated shot ids")
    parser.add_argument("--models", help="an installed model folder, cloned into the fresh profile")
    parser.add_argument("--version", help="the version the app reports (default: its Info.plist)")
    parser.add_argument("--port", type=int, default=7988)
    sys.exit(Capture(parser.parse_args()).main())


if __name__ == "__main__":
    main()
