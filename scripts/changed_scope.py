#!/usr/bin/env python3
"""Which gates of scripts/check.sh a change needs (used by `check.sh --changed`).

Compares the working tree (committed, staged, unstaged and untracked files)
with the merge base of $CHECK_BASE (default origin/main; set it to the parent
branch for stacked work) and prints shell assignments:

  FRONTEND=0|1    frontend gates (typecheck, lint, tests, i18n, build)
  RUST=0|1        Rust gates (fmt, clippy, layering, tests)
  RUST_PKGS=...   each changed workspace package and every package depending
                  on it (space-separated names); empty with RUST=1 means the
                  whole workspace (root manifests, lockfile, vendored patches, ...)
  DEPS=0|1        cargo deny (manifests, lockfile, deny.toml, vendor/)

Files it cannot place select every gate, so the scope errs on the safe side.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

FRONTEND_PREFIXES = ("src/", "public/")
FRONTEND_FILES = {
    "index.html", "package.json", "package-lock.json", "vite.config.mjs",
    "tsconfig.json", "eslint.config.js", "i18next.config.ts",
}
# Rust inputs outside any package: they can change every crate.
RUST_GLOBAL_PREFIXES = ("vendor/", ".cargo/", ".config/")
RUST_GLOBAL_FILES = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "deny.toml"}
DEPS_FILES = {"Cargo.toml", "Cargo.lock", "deny.toml"}
# Need no gate beyond the legal step check.sh always runs.
DOC_PREFIXES = ("docs/", "licenses/", "third_party/", "packaging/", ".github/")


def git(*args):
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout


def changed_files():
    try:
        base = git("merge-base", os.environ.get("CHECK_BASE", "origin/main"), "HEAD").strip()
    except subprocess.CalledProcessError:
        return None
    files = set(git("diff", "--name-only", base).split())
    files |= set(git("ls-files", "--others", "--exclude-standard").split())
    return sorted(files)


def workspace():
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
        cwd=ROOT, check=True, capture_output=True, text=True).stdout)
    dirs, users = {}, {}
    names = {p["name"] for p in meta["packages"]}
    for p in meta["packages"]:
        rel = Path(p["manifest_path"]).parent.relative_to(ROOT).as_posix()
        dirs[rel] = p["name"]
        for d in p["dependencies"]:
            if d["name"] in names:
                users.setdefault(d["name"], set()).add(p["name"])
    return dirs, users


def main():
    files = changed_files()
    frontend = rust = deps = False
    rust_all = False
    pkgs = set()
    dirs = users = None
    if files is None:
        frontend = rust = deps = rust_all = True
        files = []
    for f in files:
        name = Path(f).name
        if f in FRONTEND_FILES or f.startswith(FRONTEND_PREFIXES):
            frontend = True
            continue
        if f in DEPS_FILES or name == "Cargo.toml" or f.startswith("vendor/"):
            deps = True
        if f in RUST_GLOBAL_FILES or f.startswith(RUST_GLOBAL_PREFIXES):
            rust = rust_all = True
            continue
        if dirs is None:
            dirs, users = workspace()
        owner = max((d for d in dirs if f.startswith(d + "/")), key=len, default=None)
        if owner is not None:
            rust = True
            pkgs.add(dirs[owner])
            continue
        if f.startswith(DOC_PREFIXES) or (f.endswith(".md") and "/" not in f) or f in {"LICENSE", "NOTICE"}:
            continue
        # scripts/, unknown files: run everything.
        frontend = rust = deps = rust_all = True
    if rust and not rust_all:
        todo = list(pkgs)
        while todo:
            for u in users.get(todo.pop(), ()):
                if u not in pkgs:
                    pkgs.add(u)
                    todo.append(u)
    print(f"FRONTEND={int(frontend)}")
    print(f"RUST={int(rust)}")
    print("RUST_PKGS='" + ("" if rust_all else " ".join(sorted(pkgs))) + "'")
    print(f"DEPS={int(deps)}")


if __name__ == "__main__":
    sys.exit(main())
