#!/usr/bin/env python3
"""Collect the license texts of every third-party package Rawtonic ships.

MIT, BSD, Apache-2.0 and similar licenses require their text (with the
copyright notice) to accompany every copy, binaries included. This script
writes them into one file that the app bundles and shows in About:

- Rust: every package reachable through normal (not build or dev)
  dependencies from `rawtonic-app`, `rawtonic-cli` and the tether helper
  `rawtonic-tether` with its camera feature, on every target platform (a
  superset of any single build). Rawtonic's own crates are
  covered by LICENSE.
- npm: the production dependencies (`npm ls --omit=dev`), the code that
  ends up in the frontend bundle; build and test tools are not shipped.
- AI models: the texts in `licenses/models/` of the models and the ONNX
  Runtime the app downloads from its model pack (docs/MODELS.md). The app
  fetches only the model files, so their licenses come with the app.

Each package contributes its own license, notice and copying files. A
package that ships none is listed with its declared license and authors,
and the standard text of that license (`licenses/spdx/`, from the SPDX
license list) is added once in the appendix. Identical texts are printed
once and referred to afterwards. The script fails if a package has
neither a license file nor a license we have the standard text of.

Usage: scripts/third_party_licenses.py [--out FILE] [--check]
Default output: src-tauri/resources/licenses/THIRD_PARTY_LICENSES.txt
(generated, gitignored; `npm run licenses`, run by `tauri build`).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO / "src-tauri/resources/licenses/THIRD_PARTY_LICENSES.txt"
SPDX_DIR = REPO / "licenses/spdx"
MODEL_LICENSES = REPO / "licenses/models"
# Full texts that ship in the app's licenses folder already.
BUNDLED = {
    "AGPL-3.0": REPO / "src-tauri/resources/licenses/AGPL-3.0.txt",
    "LGPL-2.1": REPO / "src-tauri/resources/licenses/LGPL-2.1.txt",
    "LGPL-3.0": REPO / "src-tauri/resources/licenses/LGPL-3.0.txt",
}
# Which alternative of an "A OR B" expression to take, most preferred first.
PREFERENCE = [
    "MIT", "Apache-2.0", "Zlib", "BSD-3-Clause", "BSD-2-Clause", "ISC", "0BSD",
    "Unlicense", "CC0-1.0", "BSL-1.0", "Unicode-3.0", "MPL-2.0",
    "LGPL-2.1", "LGPL-3.0", "AGPL-3.0",
]
# JSON from Cargo, npm and manifests is UTF-8 (the JSON standard). Without it,
# Python decodes with the locale's encoding — cp1252 on Windows, where Cargo's
# metadata fails on the first byte cp1252 has no mapping for (0x81 etc.).
UTF8 = {"encoding": "utf-8", "errors": "strict"}
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|copyright|unlicense)", re.I)
RULE = "=" * 78


def normalise(identifier: str) -> str:
    identifier = identifier.strip().strip("()")
    identifier = re.sub(r"-(only|or-later)$", "", identifier)
    return identifier.rstrip("+")


def standard_text(identifier: str) -> Path | None:
    identifier = normalise(identifier)
    if identifier in BUNDLED:
        return BUNDLED[identifier]
    path = SPDX_DIR / f"{identifier}.txt"
    return path if path.is_file() else None


def chosen_licenses(expression: str | None) -> list[str] | None:
    """The licenses we comply with for an SPDX expression (None: unknown)."""
    if not expression:
        return None
    alternatives = [a for a in re.split(r"\s+OR\s+|/", expression) if a.strip()]
    candidates = []
    for alternative in alternatives:
        parts = [normalise(p) for p in re.split(r"\s+AND\s+", alternative.strip("() "))]
        if all(standard_text(p) for p in parts):
            rank = min(PREFERENCE.index(p) if p in PREFERENCE else len(PREFERENCE) for p in parts)
            candidates.append((rank, parts))
    return min(candidates)[1] if candidates else None


def license_files(root: Path, extra: str | None = None) -> list[Path]:
    files = sorted(p for p in root.iterdir() if p.is_file() and LICENSE_FILE.match(p.name))
    for folder in ("LICENSES", "licenses"):
        if (root / folder).is_dir():
            files += sorted(p for p in (root / folder).iterdir() if p.is_file())
    if extra and (root / extra).is_file() and root / extra not in files:
        files.append(root / extra)
    return files


def rust_packages() -> list[dict]:
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked",
         # The helper ships with libgphoto2's bindings.
         "--features", "rawtonic-tether/gphoto2"], cwd=REPO, text=True, **UTF8))
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    roots = [i for i in metadata["workspace_members"]
             if packages[i]["name"] in ("rawtonic-app", "rawtonic-cli", "rawtonic-tether")]
    seen, stack = set(), list(roots)
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes[current]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    out = []
    for package in (packages[i] for i in seen):
        if package["name"].startswith("rawtonic-"):
            continue
        out.append({
            "name": package["name"],
            "version": package["version"],
            "license": package.get("license"),
            "authors": package.get("authors") or [],
            "source": package.get("repository") or package.get("homepage") or "",
            "kind": "Rust crate",
            "root": Path(package["manifest_path"]).parent,
            "license_file": package.get("license_file"),
        })
    return out


def npm_executable() -> str:
    """npm as the system runs it: `npm.cmd` on Windows, where the bare name
    is not an executable and starting it without a shell fails (WinError 2)."""
    for name in ("npm.cmd", "npm") if os.name == "nt" else ("npm",):
        path = shutil.which(name)
        if path:
            return path
    sys.exit("npm was not found on PATH (run `npm ci` first)")


def npm_packages() -> list[dict]:
    listing = subprocess.run([npm_executable(), "ls", "--omit=dev", "--all", "--parseable"],
                             cwd=REPO, text=True, capture_output=True, **UTF8)
    # A missing or incomplete node_modules makes npm ls exit non-zero with a
    # partial (or empty) listing, which would read as "every package covered".
    if listing.returncode != 0:
        sys.exit("npm ls --omit=dev failed (run `npm ci` first):\n" + listing.stderr.strip()[-2000:])
    out, seen = [], set()
    for line in listing.stdout.splitlines()[1:]:
        root = Path(line)
        manifest = json.loads((root / "package.json").read_text(encoding="utf-8"))
        key = (manifest["name"], manifest["version"])
        if key in seen:
            continue
        seen.add(key)
        author = manifest.get("author")
        if isinstance(author, dict):
            author = author.get("name")
        repository = manifest.get("repository")
        if isinstance(repository, dict):
            repository = repository.get("url")
        license_value = manifest.get("license")
        if isinstance(license_value, dict):
            license_value = license_value.get("type")
        out.append({
            "name": manifest["name"],
            "version": manifest["version"],
            "license": license_value,
            "authors": [author] if author else [],
            "source": repository or manifest.get("homepage") or "",
            "kind": "npm package",
            "root": root,
            "license_file": None,
        })
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--check", action="store_true", help="only verify that every package is covered")
    args = parser.parse_args()

    packages = sorted(rust_packages() + npm_packages(), key=lambda p: (p["kind"], p["name"].lower(), p["version"]))
    version = json.loads((REPO / "package.json").read_text(encoding="utf-8"))["version"]
    sections, problems, appendix = [], [], set()
    first_seen: dict[str, str] = {}
    for package in packages:
        label = f"{package['name']} {package['version']}"
        lines = [RULE, f"{label} ({package['kind']})", f"License: {package['license'] or 'not declared'}"]
        if package["source"]:
            lines.append(f"Source: {package['source']}")
        files = license_files(package["root"], package["license_file"])
        if files:
            for path in files:
                text = path.read_text(encoding="utf-8", errors="replace").strip()
                digest = hashlib.sha256(text.encode()).hexdigest()
                lines += ["-" * 78, path.relative_to(package["root"]).as_posix()]
                if digest in first_seen:
                    lines.append(f"(Text identical to {first_seen[digest]}.)")
                else:
                    first_seen[digest] = f"{path.name} of {label}"
                    lines += ["", text]
        else:
            chosen = chosen_licenses(package["license"])
            if chosen is None:
                problems.append(f"{label} ({package['kind']}): no license file and no standard text for "
                                f"{package['license']!r}")
                continue
            appendix.update(chosen)
            holders = ", ".join(a.split(" <")[0] for a in package["authors"]) or f"the {package['name']} authors"
            lines += ["-" * 78,
                      f"The package ships no license file. Used under {' AND '.join(chosen)}; "
                      f"copyright holders: {holders}. The standard text is in the appendix."]
        sections.append("\n".join(lines))

    if problems:
        sys.exit("Third-party licenses incomplete:\n  " + "\n  ".join(problems))
    model_texts = sorted(MODEL_LICENSES.glob("*.txt"))
    if not model_texts:
        sys.exit(f"No model license texts in {MODEL_LICENSES.relative_to(REPO)}")
    if args.check:
        print(f"{len(packages)} packages covered ({len(appendix)} standard texts in the appendix), "
              f"{len(model_texts)} model license texts.")
        return
    models = [RULE, "AI models (downloaded from Rawtonic's model pack on first use; sources in docs/MODELS.md)"]
    for path in model_texts:
        text = path.read_text(encoding="utf-8", errors="replace").strip()
        digest = hashlib.sha256(text.encode()).hexdigest()
        models += ["-" * 78, path.name]
        if digest in first_seen:
            models.append(f"(Text identical to {first_seen[digest]}.)")
        else:
            first_seen[digest] = path.name
            models += ["", text]

    header = (f"Third-party licenses of Rawtonic {version}\n\n"
              "Rawtonic is licensed under the GNU AGPL v3 or later (AGPL-3.0.txt). It includes the following "
              "third-party packages and downloads the AI models listed after them; their license texts and "
              "notices follow. Generated by "
              "scripts/third_party_licenses.py.\n"
              f"{len(packages)} packages.")
    body = [header] + sections + ["\n".join(models), RULE, "Appendix: standard license texts"]
    for identifier in sorted(appendix):
        body += [RULE, identifier, "", standard_text(identifier).read_text(encoding="utf-8").strip()]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.out.with_suffix(".tmp")
    temporary.write_text("\n\n".join(body) + "\n", encoding="utf-8")
    os.replace(temporary, args.out)
    print(f"{args.out.relative_to(REPO) if args.out.is_relative_to(REPO) else args.out}: "
          f"{len(packages)} packages, {args.out.stat().st_size // 1024} KiB")


if __name__ == "__main__":
    main()
