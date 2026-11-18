#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""A local release feed for testing the in-app update with two QA builds
(docs/testing/in-app-update.md). Never for release builds: only a QA build
(identifier cc.rawtonic.qa*) reads RAWTONIC_QA_UPDATE_FEED, and only a QA
build configured with `dangerousInsecureTransportProtocol` fetches over http.

    update_feed.py --version 0.1.0-alpha.91 --dir PACKAGES \
        --package linux-x86_64-appimage=Rawtonic_QA_0.1.0-alpha.91_amd64.AppImage \
        [--package TARGET=FILE ...] [--installer FILE ...] [--no-manifest] [--host 127.0.0.1] [--port 8765]

PACKAGES holds each package and its `FILE.sig` (`tauri signer sign
--app-version VERSION FILE` with the QA key). The feed serves, at
http://HOST:PORT:
  /releases.json                        the GitHub API's release list (one release)
  /releases/tag/vVERSION                a placeholder release page
  /releases/download/vVERSION/NAME      the packages and latest.json
--installer lists a file of PACKAGES among the release's files without a
package key: the app offers "Install update" only when the release has an
installer for the computer (a .dmg for macOS; the AppImage and the Windows
setup are their own installers). --no-manifest leaves latest.json out (a release from before in-app updates:
the notice must fall back to the browser link). Every request is logged.
"""
import argparse
import http.server
import json
import os
import sys
import urllib.parse


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--version", required=True)
    parser.add_argument("--dir", required=True)
    parser.add_argument("--package", action="append", default=[], metavar="TARGET=FILE")
    parser.add_argument("--installer", action="append", default=[], metavar="FILE")
    parser.add_argument("--no-manifest", action="store_true")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()

    base = f"http://{args.host}:{args.port}"
    tag = f"v{args.version}"
    download = f"{base}/releases/download/{tag}/"
    files = {}
    platforms = {}
    for entry in args.package:
        target, _, name = entry.partition("=")
        path = os.path.join(args.dir, name)
        if not target or not os.path.isfile(path) or not os.path.isfile(path + ".sig"):
            sys.exit(f"{entry}: needs {path} and {path}.sig")
        with open(path + ".sig", encoding="utf-8") as f:
            platforms[target] = {"signature": f.read().strip(), "url": download + name}
        files[name] = path
    for name in args.installer:
        path = os.path.join(args.dir, name)
        if not os.path.isfile(path):
            sys.exit(f"--installer {name}: no {path}")
        files[name] = path
    manifest = json.dumps(
        {"version": args.version, "notes": f"QA feed {args.version}", "pub_date": "2026-11-18T00:00:00Z", "platforms": platforms},
        indent=2,
    ).encode()
    assets = [{"name": n, "size": os.path.getsize(p), "browser_download_url": download + n} for n, p in files.items()]
    if not args.no_manifest:
        assets.append({"name": "latest.json", "size": len(manifest), "browser_download_url": download + "latest.json"})
    releases = json.dumps(
        [
            {
                "tag_name": tag,
                "html_url": f"{base}/releases/tag/{tag}",
                "draft": False,
                "prerelease": "-" in args.version,
                "body": f"## QA feed\n\nRelease {args.version} served by scripts/qa/update_feed.py.",
                "assets": assets,
            }
        ]
    ).encode()

    class Feed(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            path = urllib.parse.urlsplit(self.path).path
            body, kind = None, "application/octet-stream"
            if path == "/releases.json":
                body, kind = releases, "application/json"
            elif path == f"/releases/tag/{tag}":
                body, kind = f"QA release {args.version}".encode(), "text/plain"
            elif path == f"/releases/download/{tag}/latest.json" and not args.no_manifest:
                body, kind = manifest, "application/json"
            elif path.startswith(f"/releases/download/{tag}/") and path.rsplit("/", 1)[1] in files:
                with open(files[path.rsplit("/", 1)[1]], "rb") as f:
                    body = f.read()
            if body is None:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    print(f"feed {base} — release {tag}, packages: {', '.join(platforms) or 'none'}"
          f"{' (no latest.json)' if args.no_manifest else ''}", flush=True)
    http.server.ThreadingHTTPServer((args.host, args.port), Feed).serve_forever()


if __name__ == "__main__":
    main()
