#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Review a first-time vendored SQLite copy against its previously pinned archive.

Only byte-identical upstream files are elided. Every local addition, edit,
deletion and executable-mode change is reported from committed Git objects.
The archive's checksum comes from the merge base, never the proposed change.
"""
from __future__ import annotations

import argparse
import difflib
import hashlib
import io
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile

VENDOR = "vendor/libsqlite3-sys"


def pinned_package(lock: str) -> dict[str, str]:
    """Read Cargo-generated lockfile scalars, rejecting unsupported syntax.

    Cargo emits these four fields as plain ASCII strings. This deliberately
    does not interpret arbitrary TOML or require another Python dependency.
    """
    pinned = []
    for block in re.split(r"(?m)^\[\[package\]\][ \t]*$", lock)[1:]:
        fields = {}
        for key in ("name", "version", "source", "checksum"):
            values = re.findall(rf"(?m)^[ \t]*{key}[ \t]*=[ \t]*([^\r\n]*)$", block)
            if len(values) > 1:
                raise ValueError("ambiguous Cargo.lock package scalar")
            if values:
                value = re.fullmatch(r'"([^"\\]*)"[ \t]*(?:#.*)?', values[0])
                if value is None:
                    raise ValueError("unsupported Cargo.lock scalar syntax")
                fields[key] = value[1]
        if fields.get("name") == "libsqlite3-sys":
            pinned.append(fields)
    if len(pinned) != 1 or pinned[0].get("source") != "registry+https://github.com/rust-lang/crates.io-index":
        raise ValueError("merge base must pin one crates.io libsqlite3-sys package")
    result = pinned[0]
    if (not re.fullmatch(r"[0-9A-Za-z.+-]+", result.get("version", ""))
            or not re.fullmatch(r"[0-9a-f]{64}", result.get("checksum", ""))):
        raise ValueError("invalid merge-base package version or checksum")
    return result


def git(*args: str) -> bytes:
    return subprocess.check_output(["git", *args], stderr=subprocess.PIPE)


def exists(commit: str, path: str) -> bool:
    return subprocess.run(
        ["git", "cat-file", "-e", f"{commit}:{path}"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ).returncode == 0


def upstream(archive: Path, checksum: str, prefix: str) -> dict[str, tuple[str, bytes]]:
    raw = archive.read_bytes()
    if hashlib.sha256(raw).hexdigest() != checksum:
        raise ValueError("upstream crate archive does not match the merge-base checksum")
    files = {}
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as package:
        for member in package:
            if member.isdir():
                continue
            name = PurePosixPath(member.name)
            if (not member.isfile() or name.is_absolute() or ".." in name.parts
                    or not member.name.startswith(prefix + "/")):
                raise ValueError("unexpected upstream crate member")
            relative = member.name[len(prefix) + 1:]
            if not relative or relative in files:
                raise ValueError("duplicate or empty upstream crate member")
            content = package.extractfile(member)
            assert content is not None
            files[relative] = ("100755" if member.mode & 0o111 else "100644", content.read())
    return files


def committed(commit: str) -> dict[str, tuple[str, bytes]]:
    files = {}
    for entry in git("ls-tree", "-r", "-z", commit, "--", VENDOR).split(b"\0"):
        if not entry:
            continue
        metadata, path = entry.split(b"\t", 1)
        mode, kind, oid = metadata.decode("ascii").split()
        if kind != "blob" or mode not in ("100644", "100755"):
            raise ValueError("non-regular vendored entry requires an uncompacted review")
        relative = path.decode("utf-8")[len(VENDOR) + 1:]
        files[relative] = (mode, git("cat-file", "blob", oid))
    return files


def render(old: dict[str, tuple[str, bytes]], new: dict[str, tuple[str, bytes]]) -> str:
    parts = []
    unchanged = 0
    for name in sorted(old.keys() | new.keys()):
        if any(ord(char) < 32 for char in name):
            raise ValueError("control characters in vendor filenames require an uncompacted review")
        before = old.get(name)
        after = new.get(name)
        if before == after:
            unchanged += 1
            continue
        # Cargo's own archive metadata and a library's private lockfile are not
        # inputs to this workspace build. Report their omission with hashes.
        if after is None and name in (".cargo_vcs_info.json", "Cargo.lock"):
            assert before is not None
            parts.append(f"Upstream packaging file omitted: {name} "
                         f"(SHA-256 {hashlib.sha256(before[1]).hexdigest()})\n")
            continue
        before_mode, before_data = before or ("absent", b"")
        after_mode, after_data = after or ("absent", b"")
        if b"\0" in before_data or b"\0" in after_data:
            raise ValueError("changed binary vendor content requires an uncompacted review")
        parts.append(f"\nVendored change: {VENDOR}/{name} "
                     f"(mode {before_mode} -> {after_mode})\n")
        parts.append(f"SHA-256 {hashlib.sha256(before_data).hexdigest()} -> "
                     f"{hashlib.sha256(after_data).hexdigest()}\n")
        if b"\r" in before_data or b"\r" in after_data:
            parts.append("CRLF line endings: upstream " + str(before_data.count(b"\r\n"))
                         + ", proposed " + str(after_data.count(b"\r\n")) + "\n")
        delta = difflib.unified_diff(
            before_data.decode("utf-8").splitlines(keepends=True),
            after_data.decode("utf-8").splitlines(keepends=True),
            fromfile=f"upstream/{name}" if before else "/dev/null",
            tofile=f"{VENDOR}/{name}" if after else "/dev/null",
        )
        for line in delta:
            parts.append(line if line.endswith("\n") else line + "\n")
        for label, data in (("upstream", before_data), ("proposed", after_data)):
            if data and not data.endswith(b"\n"):
                parts.append(f"No final newline in {label} file.\n")
    return f"Byte-identical upstream files verified and elided: {unchanged}\n" + "".join(parts)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--archive", type=Path, help="optional checksum-verified .crate archive")
    args = parser.parse_args()
    head = git("rev-parse", "--verify", args.head + "^{commit}").decode().strip()
    base = git("merge-base", args.base, head).decode().strip()
    if exists(base, VENDOR) or not exists(head, VENDOR + "/Cargo.toml"):
        sys.stdout.buffer.write(git("diff", "--no-ext-diff", "--no-textconv", base, head))
        return
    pinned = pinned_package(git("show", f"{base}:Cargo.lock").decode("utf-8"))
    version, checksum = pinned["version"], pinned["checksum"]
    archive = args.archive
    if archive is None:
        cargo = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
        candidates = list(cargo.glob(f"registry/cache/*/libsqlite3-sys-{version}.crate"))
        archive = next((p for p in candidates if hashlib.sha256(p.read_bytes()).hexdigest() == checksum), None)
    if archive is None:
        raise ValueError("pinned upstream archive missing; run cargo fetch or pass --archive")
    original = upstream(archive, checksum, f"libsqlite3-sys-{version}")
    report = render(original, committed(head))
    rest = git("diff", "--no-ext-diff", "--no-textconv", base, head, "--", ".", f":(exclude){VENDOR}/**")
    # Nothing is emitted until all verification and complete delta generation
    # succeed. An error cannot leave a deceptively partial review prompt.
    sys.stdout.buffer.write(rest)
    sys.stdout.flush()
    print(f"\n## Verified vendored upstream delta at {head}\n"
          f"Merge-base pin: libsqlite3-sys {version}, archive SHA-256 {checksum}\n"
          "All other branch changes appear above. The files remain available "
          "for read-only inspection; no vendor path is hidden by Git attributes.\n" + report)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, UnicodeError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print(f"review: cannot verify review input: {error}", file=sys.stderr)
        sys.exit(1)
