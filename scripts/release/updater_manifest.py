#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The in-app update manifest (`latest.json`) of one release.

    updater_manifest.py make --version V --base-url URL --pub-date DATE --notes TEXT \
        --sig-dir DIR --out latest.json TARGET=FILE...
    updater_manifest.py verify --manifest latest.json --pubkey KEY --version V \
        --base-url URL --dir ASSETS [--names FILE] [--expect TARGET...]

`make` writes the manifest Tauri's updater reads: the version, a short note,
the date and, per package key (`darwin-aarch64-app`, `linux-x86_64-appimage`,
`windows-x86_64-nsis`, ...), the package's URL among the release's files and
its signature (`FILE.sig` from `tauri signer sign --app-version V`, in
--sig-dir).

`verify` checks a manifest against the release: its version, every URL inside
the release's own download folder and named among --dir's files (and among
the draft's asset names in --names, one per line), every signature made by
the key whose public half the app carries (--pubkey: the base64 string from
tauri.conf.json `plugins.updater.pubkey`) over that file, bound to this
version, and the --expect keys present. The signature check is minisign's
(Ed25519 over the file's BLAKE2b-512, then over the signature and its trusted
comment), written out here so the release machine needs no extra tool.
"""
import argparse
import base64
import hashlib
import json
import os
import sys

# --- Ed25519 verification (RFC 8032, section 5.1.7) --------------------------
# Adapted from the Python reference code in RFC 8032, section 6 (S. Josefsson,
# I. Liusvaara), a Code Component under the IETF Trust's Revised BSD License:
#
# Copyright (c) 2017 IETF Trust and the persons identified as authors of the
# code. All rights reserved.
#
# Redistribution and use in source and binary forms, with or without
# modification, are permitted provided that the following conditions are met:
# - Redistributions of source code must retain the above copyright notice,
#   this list of conditions and the following disclaimer.
# - Redistributions in binary form must reproduce the above copyright notice,
#   this list of conditions and the following disclaimer in the documentation
#   and/or other materials provided with the distribution.
# - Neither the name of Internet Society, IETF or IETF Trust, nor the names of
#   specific contributors, may be used to endorse or promote products derived
#   from this software without specific prior written permission.
#
# THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
# AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
# IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
# ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
# LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
# CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
# SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
# INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
# CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
# ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
# POSSIBILITY OF SUCH DAMAGE.
P = 2**255 - 19
Q = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
SQRT_M1 = pow(2, (P - 1) // 4, P)


def _add(a, b):
    x = (a[1] - a[0]) * (b[1] - b[0]) % P
    y = (a[1] + a[0]) * (b[1] + b[0]) % P
    c = 2 * a[3] * b[3] * D % P
    z = 2 * a[2] * b[2] % P
    e, f, g, h = y - x, z - c, z + c, y + x
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def _mul(s, pt):
    acc = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            acc = _add(acc, pt)
        pt = _add(pt, pt)
        s >>= 1
    return acc


def _equal(a, b):
    return (a[0] * b[2] - b[0] * a[2]) % P == 0 and (a[1] * b[2] - b[1] * a[2]) % P == 0


def _recover_x(y, sign):
    if y >= P:
        return None
    x2 = (y * y - 1) * pow(D * y * y + 1, P - 2, P) % P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P:
        x = x * SQRT_M1 % P
    if (x * x - x2) % P:
        return None
    return P - x if (x & 1) != sign else x


def _decompress(raw):
    if len(raw) != 32:
        return None
    y = int.from_bytes(raw, "little")
    sign, y = y >> 255, y & ((1 << 255) - 1)
    x = _recover_x(y, sign)
    return None if x is None else (x, y, 1, x * y % P)


_GY = 4 * pow(5, P - 2, P) % P
_GX = _recover_x(_GY, 0)
G = (_GX, _GY, 1, _GX * _GY % P)


def ed25519_verify(public, message, signature):
    a = _decompress(public)
    r = _decompress(signature[:32])
    if a is None or r is None or len(signature) != 64:
        return False
    s = int.from_bytes(signature[32:], "little")
    if s >= Q:
        return False
    h = int.from_bytes(hashlib.sha512(signature[:32] + public + message).digest(), "little") % Q
    return _equal(_mul(s, G), _add(r, _mul(h, a)))


# --- minisign as Tauri writes it ---------------------------------------------
def _b64_lines(text):
    return base64.b64decode(text.strip()).decode("utf-8").splitlines()


def public_key(pubkey_b64):
    """(key id, 32-byte key) from tauri.conf.json's `pubkey`."""
    raw = base64.b64decode(_b64_lines(pubkey_b64)[1])
    if len(raw) != 42 or raw[:2] != b"Ed":
        raise ValueError("not a minisign Ed25519 public key")
    return raw[2:10], raw[10:]


def check_signature(data, signature_b64, pubkey_b64, version):
    """None when `signature_b64` (a `.sig` file's content) is the key's
    signature of `data` bound to `version`, else the reason."""
    key_id, key = public_key(pubkey_b64)
    lines = _b64_lines(signature_b64)
    if len(lines) < 4 or not lines[2].startswith("trusted comment: "):
        return "malformed signature"
    raw = base64.b64decode(lines[1])
    if len(raw) != 74 or raw[:2] not in (b"ED", b"Ed"):
        return "unknown signature algorithm"
    if raw[2:10] != key_id:
        return "signed by another key"
    message = hashlib.blake2b(data, digest_size=64).digest() if raw[:2] == b"ED" else data
    if not ed25519_verify(key, message, raw[10:]):
        return "signature does not match the file"
    trusted = lines[2][len("trusted comment: "):]
    if not ed25519_verify(key, raw[10:] + trusted.encode("utf-8"), base64.b64decode(lines[3])):
        return "trusted comment not signed"
    fields = dict(f.split(":", 1) for f in trusted.split("\t") if ":" in f)
    if fields.get("version", "").lstrip("v") != version:
        return f"signed for version {fields.get('version')!r}, not {version}"
    return None


# --- commands ----------------------------------------------------------------
def make(args):
    platforms = {}
    for entry in args.entries:
        target, _, path = entry.partition("=")
        if not target or not path:
            sys.exit(f"bad entry {entry!r} (TARGET=FILE)")
        name = os.path.basename(path)
        with open(os.path.join(args.sig_dir, name + ".sig"), encoding="utf-8") as f:
            signature = f.read().strip()
        platforms[target] = {"signature": signature, "url": args.base_url + name}
    manifest = {
        "version": args.version,
        "notes": args.notes,
        "pub_date": args.pub_date,
        "platforms": platforms,
    }
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")
    print(f"{args.out}: {', '.join(sorted(platforms))}")


def verify(args):
    with open(args.manifest, encoding="utf-8") as f:
        manifest = json.load(f)
    names = None
    if args.names:
        with open(args.names, encoding="utf-8") as f:
            names = {line.strip() for line in f if line.strip()}
    bad = []
    if manifest.get("version") != args.version:
        bad.append(f"version {manifest.get('version')!r}, not {args.version}")
    platforms = manifest.get("platforms") or {}
    for target in args.expect or []:
        if target not in platforms:
            bad.append(f"no package for {target}")
    for target, entry in sorted(platforms.items()):
        url = entry.get("url", "")
        name = url[len(args.base_url):] if url.startswith(args.base_url) else None
        if not name or "/" in name:
            bad.append(f"{target}: {url} is not a file of this release")
            continue
        if names is not None and name not in names:
            bad.append(f"{target}: {name} is not among the draft's assets")
        path = os.path.join(args.dir, name)
        if not os.path.isfile(path):
            bad.append(f"{target}: {name} not found in {args.dir}")
            continue
        with open(path, "rb") as f:
            data = f.read()
        why = check_signature(data, entry.get("signature", ""), args.pubkey, args.version)
        if why:
            bad.append(f"{target}: {name}: {why}")
        else:
            print(f"{target}: {name} ({len(data)} bytes): signature ok, version {args.version}")
    for line in bad:
        print("BAD " + line)
    if bad or not platforms:
        sys.exit(1)
    print(f"manifest ok: {len(platforms)} packages")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    m = sub.add_parser("make")
    m.add_argument("--version", required=True)
    m.add_argument("--base-url", required=True)
    m.add_argument("--pub-date", required=True)
    m.add_argument("--notes", required=True)
    m.add_argument("--sig-dir", required=True)
    m.add_argument("--out", required=True)
    m.add_argument("entries", nargs="+")
    v = sub.add_parser("verify")
    v.add_argument("--manifest", required=True)
    v.add_argument("--pubkey", required=True)
    v.add_argument("--version", required=True)
    v.add_argument("--base-url", required=True)
    v.add_argument("--dir", required=True)
    v.add_argument("--names")
    v.add_argument("--expect", nargs="*")
    args = parser.parse_args()
    (make if args.command == "make" else verify)(args)


if __name__ == "__main__":
    main()
