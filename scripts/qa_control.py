#!/usr/bin/env python3
"""Client for the QA control channel of a qa-control build (src-tauri/src/qa_control.rs).

Build:  VITE_RAWTONIC_QA=1 npm run tauri -- build --features qa-control --bundles app
        (add VITE_RAWTONIC_PERF=1 and performance-profiling for latency runs)
Launch: open --env RAWTONIC_QA_PORT=7979 path/to/Rawtonic.app

    scripts/qa_control.py call ping
    scripts/qa_control.py call ui.elements '{"name": "export"}'
    scripts/qa_control.py call state.get '{"path": "editor.selectedImage.path"}'
    scripts/qa_control.py --activate click --name "Layers & masks"  # native click at the centre
    scripts/qa_control.py drag 400,300 600,300 --steps 30 --interval-ms 16.7
    scripts/qa_control.py screenshot out.png                # macOS: the app captures its own window

Coordinates are CSS pixels from the webview's top left. Native input (click, drag) is
macOS-only; elsewhere use `call ui.click` / `call ui.drag` (synthetic DOM events).
"""
import argparse
import base64
import json
import socket
import subprocess
import sys
import time


class QaControl:
    def __init__(self, port=7979, host="127.0.0.1", timeout=60.0):
        self.port = port
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.reader = self.sock.makefile("r", encoding="utf-8")
        self.next_id = 1

    def call(self, method, params=None):
        request = {"id": self.next_id, "method": method, "params": params or {}}
        self.next_id += 1
        self.sock.sendall((json.dumps(request) + "\n").encode())
        reply = json.loads(self.reader.readline())
        if reply.get("error") is not None:
            raise RuntimeError(f"{method}: {reply['error']}")
        return reply.get("result")

    def find(self, **query):
        return self.call("ui.find", query)

    def bring_to_front(self, timeout=3.0):
        """Activate the app and wait until its window has focus. AppKit activation is
        asynchronous, and since macOS 14 an app cannot always activate itself while
        another app is frontmost; AppleScript activation is the fallback."""
        self.call("input.mouse", {"type": "move", "x": 1, "y": 1, "activate": True})
        deadline = time.monotonic() + timeout
        asked_system = False
        while time.monotonic() < deadline:
            if self.call("window.info")["focused"]:
                return
            if not asked_system and time.monotonic() > deadline - timeout / 2:
                self._activate_by_pid()
                asked_system = True
            time.sleep(0.05)
        raise RuntimeError("the app window did not become active")

    def _activate_by_pid(self):
        """Activates the process that serves this port, whatever its bundle identifier
        (QA builds run as cc.rawtonic.qa beside the user's cc.rawtonic.editor)."""
        pids = subprocess.run(
            ["lsof", "-nP", f"-iTCP:{self.port}", "-sTCP:LISTEN", "-t"],
            capture_output=True, text=True, check=False,
        ).stdout.split()
        if pids:
            subprocess.run(
                [
                    "osascript", "-e",
                    'tell application "System Events" to set frontmost of '
                    f"(first process whose unix id is {int(pids[0])}) to true",
                ],
                check=False,
            )

    def click(self, count=1, modifiers=(), activate=False, button="left", **query):
        if activate:
            self.bring_to_front()
        element = self.find(**query)
        x, y = element["x"] + element["width"] / 2, element["y"] + element["height"] / 2
        self.call(
            "input.click",
            {
                "x": x,
                "y": y,
                "count": count,
                "modifiers": list(modifiers),
                "activate": activate,
                "button": button,
            },
        )
        return element

    def drag(self, points, interval_ms=1000 / 60, modifiers=(), activate=False):
        if activate:
            self.bring_to_front()
        return self.call(
            "input.drag",
            {
                "points": [list(p) for p in points],
                "interval_ms": interval_ms,
                "modifiers": list(modifiers),
                "activate": activate,
            },
        )

    def screenshot(self, path, resolution="nominal", color_space="srgb"):
        """The app captures its own window (macOS) as the window server composites it,
        including the native GPU layer under the transparent webview. "nominal" = points,
        "best" = backing pixels; color_space "p3" keeps colours beyond sRGB."""
        result = self.call("window.capture", {"resolution": resolution, "color_space": color_space})
        with open(path, "wb") as file:
            file.write(base64.b64decode(result["png_base64"]))
        return {"path": path, "width": result["width"], "height": result["height"]}


def line(start, end, steps):
    (x0, y0), (x1, y1) = start, end
    return [(x0 + (x1 - x0) * i / steps, y0 + (y1 - y0) * i / steps) for i in range(steps + 1)]


def parse_point(text):
    x, y = text.split(",")
    return float(x), float(y)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", type=int, default=7979)
    parser.add_argument(
        "--activate", action="store_true", help="bring the app to the front before native input"
    )
    sub = parser.add_subparsers(dest="command", required=True)
    call = sub.add_parser("call")
    call.add_argument("method")
    call.add_argument("params", nargs="?", default="{}")
    click = sub.add_parser("click")
    click.add_argument("--name", required=True)
    click.add_argument("--index", type=int, default=0)
    click.add_argument("--exact", action="store_true")
    click.add_argument("--count", type=int, default=1)
    drag = sub.add_parser("drag")
    drag.add_argument("start", type=parse_point)
    drag.add_argument("end", type=parse_point)
    drag.add_argument("--steps", type=int, default=30)
    drag.add_argument("--interval-ms", type=float, default=1000 / 60)
    drag.add_argument("--modifiers", nargs="*", default=[])
    shot = sub.add_parser("screenshot")
    shot.add_argument("path")
    shot.add_argument("--best", action="store_true", help="backing pixels instead of points")
    shot.add_argument("--p3", action="store_true", help="pixels in Display P3 instead of sRGB")
    args = parser.parse_args()

    qa = QaControl(args.port)
    if args.command == "call":
        result = qa.call(args.method, json.loads(args.params))
    elif args.command == "click":
        result = qa.click(
            count=args.count, activate=args.activate, name=args.name, index=args.index, exact=args.exact
        )
    elif args.command == "drag":
        result = qa.drag(
            line(args.start, args.end, args.steps), args.interval_ms, args.modifiers, args.activate
        )
    else:
        result = qa.screenshot(
            args.path, "best" if args.best else "nominal", "p3" if args.p3 else "srgb"
        )
    json.dump(result, sys.stdout, indent=2, ensure_ascii=False)
    print()


if __name__ == "__main__":
    main()
