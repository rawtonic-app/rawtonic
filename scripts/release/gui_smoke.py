#!/usr/bin/env python3
"""Scripted GUI smoke of a release on a fresh profile, through the QA control
channel (scripts/qa_control.py, docs/testing/qa-control.md). macOS.

    scripts/release/gui_smoke.py --app "Rawtonic QA.app" --cli target/release/rawtonic \
        --raw photo.CR3 --version 0.1.0-alpha.12 --work DIR [--port 7989]

The app must be a qa-control build (VITE_RAWTONIC_QA=1) with a QA identifier,
never the user's cc.rawtonic.editor. Everything happens in a new folder
DIR/run-<time>: HOME (so the profile is fresh: settings, library, logs), a
session made with the CLI holding a copy of the RAW. Steps:
  1. first start on the empty profile; quit through macOS (exit 0)
  2. start as after an update (lastRunVersion 0.0.1): What's new names VERSION
  3. the session opens; a double click opens the photo in Develop and a frame
     renders (window capture of the viewer: not blank)
  4. one export into the session's Output folder (a JPEG)
  5. quit through macOS: exit 0, no panic in the app log, the RAW unchanged
The app takes the keyboard focus while it runs (about a minute).
"""
import argparse
import hashlib
import json
import os
import plistlib
import socket
import struct
import subprocess
import sys
import time
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from qa_control import QaControl  # noqa: E402

USER_IDENTIFIER = "cc.rawtonic.editor"


class StepFailed(RuntimeError):
    pass


class Smoke:
    def __init__(self, args):
        self.args = args
        self.app = Path(args.app)
        info = plistlib.loads((self.app / "Contents/Info.plist").read_bytes())
        self.identifier = info["CFBundleIdentifier"]
        if self.identifier == USER_IDENTIFIER:
            raise SystemExit("gui smoke: refusing the user's app identifier; build with a QA identifier")
        self.binary = self.app / "Contents/MacOS" / info["CFBundleExecutable"]
        self.run = Path(args.work) / time.strftime("run-%Y%m%d-%H%M%S")
        self.home = self.run / "home"
        self.session = self.run / "session"
        self.config = self.home / "Library/Application Support" / self.identifier
        self.proc = None
        self.qa = None
        self.results = []

    def step(self, name, ok, detail=""):
        self.results.append({"step": name, "ok": bool(ok), "detail": detail})
        print(f"{'ok  ' if ok else 'FAIL'} {name}{': ' + detail if detail else ''}", flush=True)
        if not ok:
            raise StepFailed(f"{name}: {detail}")

    # --- app lifecycle -------------------------------------------------------
    def port_free(self):
        with socket.socket() as s:
            return s.connect_ex(("127.0.0.1", self.args.port)) != 0

    def launch(self, label):
        if not self.port_free():
            raise RuntimeError(f"port {self.args.port} is in use (another QA app?)")
        env = {**os.environ, "HOME": str(self.home), "RAWTONIC_QA_PORT": str(self.args.port)}
        log = open(self.run / f"{label}.stdout.log", "wb")
        self.proc = subprocess.Popen([str(self.binary)], env=env, stdout=log, stderr=subprocess.STDOUT,
                                     stdin=subprocess.DEVNULL)
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"the app exited at start (exit {self.proc.returncode}; another instance?)")
            try:
                self.qa = QaControl(self.args.port, timeout=60)
                version = self.qa.call("ping")["version"]
                break
            except OSError:
                time.sleep(0.5)
        else:
            raise RuntimeError("the QA channel did not answer within 90 s")
        # The native side answers first; the webview's bridge once the page has loaded.
        while time.monotonic() < deadline:
            try:
                self.qa.call("ui.viewport")
                return version
            except RuntimeError as error:
                if "no webview reply" not in str(error):
                    raise
                time.sleep(0.5)
        raise RuntimeError("the webview's QA bridge did not answer (not a VITE_RAWTONIC_QA=1 build?)")

    def quit(self):
        """Quit through macOS (an Apple Event, as the menu or Dock do); returns the exit code."""
        subprocess.run(["osascript", "-e", f'tell application id "{self.identifier}" to quit'],
                       capture_output=True, timeout=30, check=False)
        try:
            code = self.proc.wait(timeout=45)
        except subprocess.TimeoutExpired:
            code = None
        if self.qa:
            self.qa.sock.close()
            self.qa = None
        return code

    def kill(self):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def wait_state(self, path, equals=None, timeout_ms=60000):
        params = {"path": path, "timeout_ms": timeout_ms}
        if equals is not None:
            params["equals"] = equals
        return self.qa.call("state.wait", params)

    # --- steps ---------------------------------------------------------------
    def prepare(self):
        self.run.mkdir(parents=True)
        self.home.mkdir()
        raw = Path(self.args.raw)
        self.raw_sha = hashlib.sha256(raw.read_bytes()).hexdigest()
        cli = self.args.cli
        for cmd in ([cli, "create", str(self.session), "--name", "Release smoke"],
                    [cli, "ingest", str(self.session), str(raw)]):
            r = subprocess.run(cmd, capture_output=True, text=True, env={**os.environ, "HOME": str(self.home)})
            if r.returncode:
                raise RuntimeError(f"{' '.join(cmd[1:2])} failed: {r.stderr.strip()[-300:]}")
        self.photo = next((self.session / "Capture").glob(raw.name), None)
        self.step("scratch session with a copy of the RAW", self.photo is not None, str(self.session))
        self.config.mkdir(parents=True)
        (self.config / "last-library.json").write_text(json.dumps(str(self.session)))

    def first_start(self):
        version = self.launch("start1")
        self.step("starts on a fresh profile", version == self.args.version, f"version {version}")
        self.wait_state("library.sessionDirectory", str(self.session))
        code = self.quit()
        self.step("quits cleanly after the first start", code == 0, f"exit {code}")
        settings = json.loads((self.config / "settings.json").read_text())
        self.step("first start records the version", settings.get("lastRunVersion") == self.args.version,
                  str(settings.get("lastRunVersion")))
        # As after an update from an older version.
        settings["lastRunVersion"] = "0.0.1"
        tmp = self.config / "settings.json.tmp"
        tmp.write_text(json.dumps(settings, indent=2))
        tmp.replace(self.config / "settings.json")

    def whats_new(self):
        self.launch("start2")
        self.qa.call("ui.wait", {"selector": "#app-notes-title", "timeout_ms": 30000})
        title = self.qa.find(selector="#app-notes-title")["name"]
        body = self.qa.call("ui.text", {"selector": ".app-notes-body"}) or ""
        self.step("What's new shows the new version", self.args.version in title and len(body) > 100,
                  f"{title!r}, {len(body)} characters of notes")
        self.qa.call("ui.click", {"selector": ".app-notice-close"})

    def develop(self):
        self.wait_state("library.sessionDirectory", str(self.session))
        self.wait_state("library.imageList.0.path", None, 60000)
        images = self.qa.call("state.get", {"path": "library.imageList", "max_array": 10}) or []
        paths = [i.get("path") for i in images if isinstance(i, dict)]
        self.step("the session opens with the photo", str(self.photo) in paths, f"{len(paths)} photo(s)")
        tiles = [e for e in self.qa.call("ui.elements", {"name": self.photo.name}) if e["tag"] == "button"]
        if not tiles:
            raise RuntimeError("no tile for the photo")
        t = tiles[0]
        self.qa.call("input.click", {"x": t["x"] + t["width"] / 2, "y": t["y"] + t["height"] / 2, "count": 2})
        self.wait_state("ui.activeView", "editor", 20000)
        self.wait_state("editor.hasRenderedFirstFrame", True, 60000)
        selected = self.qa.call("state.get", {"path": "editor.selectedImage.path"})
        time.sleep(3)  # the full-quality frame after the first one
        shot = self.run / "develop.png"
        self.qa.screenshot(str(shot))
        stats = viewer_stats(shot.read_bytes())
        self.step("Develop renders a frame", selected == str(self.photo) and stats["ok"],
                  f"{Path(str(selected)).name}; viewer luminance sd {stats['sd']:.1f}, "
                  f"{stats['colors']} colours, top colour {stats['top_share']:.0%}")

    def export(self):
        self.qa.call("ui.click", {"name": "Export photos"})
        self.qa.call("ui.wait", {"selector": ".studio-export-submit", "timeout_ms": 10000})
        self.qa.call("ui.click", {"selector": ".studio-export-submit"})
        self.wait_state("process.exportState.status", "success", 180000)
        files = sorted((self.session / "Output").rglob("*.jpg"))
        data = files[0].read_bytes() if files else b""
        self.step("one export into the session's Output folder", len(data) > 100_000 and data[:2] == b"\xff\xd8",
                  f"{files[0].name if files else 'no file'}, {len(data)} bytes")

    def finish(self):
        code = self.quit()
        self.step("quits cleanly", code == 0, f"exit {code}")
        log = self.home / "Library/Logs" / self.identifier / "app.log"
        text = log.read_text(errors="replace") if log.exists() else ""
        panics = sum(1 for line in text.splitlines() if "panicked" in line or "[PANIC]" in line)
        self.step("no panic in the app log", log.exists() and panics == 0, f"{panics} panic line(s)")
        same = hashlib.sha256(Path(self.args.raw).read_bytes()).hexdigest() == self.raw_sha
        self.step("the original RAW is unchanged", same)

    def main(self):
        try:
            self.prepare()
            self.first_start()
            self.whats_new()
            self.develop()
            self.export()
            self.finish()
        except Exception as error:  # report, close the app, fail
            if not isinstance(error, StepFailed):
                print(f"FAIL {error}", flush=True)
            if self.qa:
                try:
                    self.qa.screenshot(str(self.run / "failure.png"))
                except Exception:
                    pass
            if self.proc and self.proc.poll() is None:
                self.quit()
            self.kill()
            ok = False
        else:
            ok = True
        (self.run / "result.json").write_text(json.dumps({"ok": ok, "steps": self.results}, indent=2))
        print(f"gui smoke: {'ok' if ok else 'FAILED'} ({self.run})")
        return 0 if ok else 1


def png_rgb(data):
    """Rows of RGB(A) bytes of an 8-bit PNG (no interlace)."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos, idat, width = 8, b"", 0
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        chunk = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", chunk)
            if depth != 8 or color not in (2, 6) or interlace:
                raise ValueError("unsupported PNG")
            bpp = 4 if color == 6 else 3
        elif kind == b"IDAT":
            idat += chunk
        pos += 12 + length
    raw = zlib.decompress(idat)
    stride = width * bpp
    rows, prev = [], bytearray(stride)
    for y in range(height):
        f = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        if f == 1:
            for i in range(bpp, stride):
                line[i] = (line[i] + line[i - bpp]) & 255
        elif f == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 255
        elif f == 3:
            for i in range(stride):
                left = line[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 255
        elif f == 4:
            for i in range(stride):
                a = line[i - bpp] if i >= bpp else 0
                b = prev[i]
                c = prev[i - bpp] if i >= bpp else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(line)
        prev = line
    return width, height, bpp, rows


def viewer_stats(png):
    """Is the middle of the viewer (where a fitted photo lies) a picture, not one flat colour?"""
    width, height, bpp, rows = png_rgb(png)
    counts, lum = {}, []
    for y in range(int(height * 0.2), int(height * 0.7), 4):
        row = rows[y]
        for x in range(int(width * 0.45), int(width * 0.85), 4):
            r, g, b = row[x * bpp], row[x * bpp + 1], row[x * bpp + 2]
            counts[(r, g, b)] = counts.get((r, g, b), 0) + 1
            lum.append(0.2126 * r + 0.7152 * g + 0.0722 * b)
    mean = sum(lum) / len(lum)
    sd = (sum((v - mean) ** 2 for v in lum) / len(lum)) ** 0.5
    top = max(counts.values()) / len(lum)
    return {"sd": sd, "colors": len(counts), "top_share": top, "ok": sd > 3 and len(counts) > 200 and top < 0.5}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--app", required=True)
    parser.add_argument("--cli", required=True, help="the rawtonic CLI of the same commit (session + ingest)")
    parser.add_argument("--raw", required=True, help="a RAW to copy into the scratch session (read only)")
    parser.add_argument("--version", required=True, help="the version the app must report")
    parser.add_argument("--work", required=True)
    parser.add_argument("--port", type=int, default=7989)
    sys.exit(Smoke(parser.parse_args()).main())


if __name__ == "__main__":
    main()
