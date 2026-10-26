#!/usr/bin/env python3
"""Opt-in Developer ID packaging; no credentials retains the ad-hoc path.

`--arch x86_64` builds the separate Intel app and DMG (also on Apple Silicon,
as a cross build): target/x86_64-apple-darwin/release/bundle/, DMG
Rawtonic_<version>_x86_64.dmg. Without --arch the machine's own build.
"""

import argparse
import base64
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import tempfile


REPO = Path(__file__).resolve().parent.parent
API = ("APPLE_API_ISSUER", "APPLE_API_KEY", "APPLE_API_KEY_PATH")
APPLE_ID = ("APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID")
# Rust architecture -> (Mach-O CPU type, Tauri's DMG suffix, our DMG suffix).
ARCHES = {"aarch64": (0x0100000C, "aarch64", "aarch64"),
          "x86_64": (0x01000007, "x64", "x86_64")}
MACHO = (b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xcf",
         b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",
         b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca")
SHA1 = re.compile(r"[0-9a-fA-F]{40}")


def run(args, *, capture=False, env=None):
    # Do not let CalledProcessError print command arguments containing secrets.
    result = subprocess.run(args, cwd=REPO, env=env, check=False,
                            stdout=subprocess.PIPE if capture else None,
                            stderr=subprocess.PIPE if capture else None, text=True)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed (exit {result.returncode})")
    return result.stdout if capture else None


def configuration(env):
    identity = env.get("APPLE_SIGNING_IDENTITY", "")
    certificate = bool(env.get("APPLE_CERTIFICATE"))
    if certificate != bool(env.get("APPLE_CERTIFICATE_PASSWORD")):
        raise ValueError("Set both APPLE_CERTIFICATE and APPLE_CERTIFICATE_PASSWORD")
    if (identity and identity != "-" and not identity.startswith("Developer ID Application:")
            and not SHA1.fullmatch(identity)):
        raise ValueError("APPLE_SIGNING_IDENTITY must be a Developer ID Application name or 40-hex SHA-1 hash")
    if certificate and identity == "-":
        raise ValueError("An Apple certificate cannot be combined with the ad-hoc identity")
    signed = certificate or bool(identity and identity != "-")
    auth = []
    for names, flags in ((API, ("--issuer", "--key-id", "--key")),
                         (APPLE_ID, ("--apple-id", "--password", "--team-id"))):
        if any(env.get(name) for name in names):
            if not all(env.get(name) for name in names):
                raise ValueError(f"Set all of {', '.join(names)}")
            if auth:
                raise ValueError("Choose API key or Apple ID notarization credentials, not both")
            auth = [item for name, flag in zip(names, flags) for item in (flag, env[name])]
    if auth and not signed:
        raise ValueError("Notarization requires Developer ID signing credentials")
    if auth and env.get("APPLE_API_KEY_PATH") and not Path(env["APPLE_API_KEY_PATH"]).is_file():
        raise ValueError("APPLE_API_KEY_PATH must name the downloaded private .p8 key")
    if signed and not certificate:
        identities = signing_identities()
        resolve_identity(identity, identities)
        if SHA1.fullmatch(identity):
            identity = identity.upper()
    return signed, identity, auth


def signing_identities(keychain=None):
    """Only valid Developer ID Application identities, without logging keychain output."""
    command = ["security", "find-identity", "-v", "-p", "codesigning"]
    if keychain:
        command.append(str(keychain))
    output = run(command, capture=True)
    return [(match[1].upper(), match[2]) for line in output.splitlines()
            if (match := re.fullmatch(
                r'\s*\d+\)\s+([0-9a-fA-F]{40})\s+"(Developer ID Application:[^"]+)"\s*', line))]


def resolve_identity(identity, identities):
    """Require one exact name/hash match; never include user input in errors."""
    is_hash = bool(SHA1.fullmatch(identity))
    matches = [item for item in identities
               if (item[0] == identity.upper() if is_hash else item[1] == identity)]
    if len(matches) > 1:
        raise ValueError("APPLE_SIGNING_IDENTITY matches multiple valid identities; "
                         "use the 40-hex SHA-1 hash from security find-identity -v -p codesigning")
    if not matches:
        raise ValueError("APPLE_SIGNING_IDENTITY must match exactly one valid Developer ID Application "
                         "identity from security find-identity -v -p codesigning")
    return matches[0][1]


def import_certificate(env, directory):
    """Temporary keychain only; never change the user's search list/default."""
    certificate = directory / "certificate.p12"
    certificate.write_bytes(base64.b64decode(env["APPLE_CERTIFICATE"], validate=True))
    keychain = directory / "signing.keychain-db"
    password = os.urandom(24).hex()
    run(["security", "create-keychain", "-p", password, str(keychain)], capture=True)
    try:
        run(["security", "unlock-keychain", "-p", password, str(keychain)], capture=True)
        run(["security", "import", str(certificate), "-k", str(keychain), "-P",
             env["APPLE_CERTIFICATE_PASSWORD"], "-T", "/usr/bin/codesign"], capture=True)
        run(["security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:",
             "-s", "-k", password, str(keychain)], capture=True)
        identities = signing_identities(keychain)
        if len(identities) != 1:
            raise ValueError("APPLE_CERTIFICATE must contain one Developer ID Application identity")
        identity = env.get("APPLE_SIGNING_IDENTITY")
        if identity:
            resolve_identity(identity, identities)
        # Tauri 2.12.1 compares APPLE_SIGNING_IDENTITY to the imported name.
        # Its own isolated keychain has only this identity, so the name is safe.
        return keychain, identities[0][1]
    except BaseException:
        run(["security", "delete-keychain", str(keychain)], capture=True)
        raise
    finally:
        certificate.unlink()


def cpu_types(path):
    """Mach-O CPU types of a file (several for a universal one), or None."""
    data = path.read_bytes()[:4096]
    if data[:4] not in MACHO:
        return None
    if data[:4] in (b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
        size = 20 if data[3] == 0xBE else 32
        count = struct.unpack(">I", data[4:8])[0]
        if count > 16:  # 0xcafebabe also starts Java class files
            return None
        return {struct.unpack(">I", data[8 + i * size:12 + i * size])[0] for i in range(count)}
    return {struct.unpack("<I" if data[0] in (0xCE, 0xCF) else ">I", data[4:8])[0]}


def check_architectures(app, arch):
    """Every native file in the bundle runs on `arch` alone: a cross build
    must not pick up the other architecture's ONNX Runtime or helper."""
    if not app.is_dir():
        raise RuntimeError(f"{app} was not built")
    wrong = [f"{path.relative_to(app)}: {sorted(hex(t) for t in types)}"
             for path in sorted(app.rglob("*")) if path.is_file() and not path.is_symlink()
             for types in [cpu_types(path)] if types is not None and types != {ARCHES[arch][0]}]
    if wrong:
        raise RuntimeError(f"not {arch}-only: " + "; ".join(wrong))


def stage_resources(target, identity, keychain, tether_dir):
    # Tauri signs externalBin and frameworks, but not Mach-O resources (.so
    # camera drivers and ONNX Runtime). Sign copies: build.rs verifies the
    # upstream runtime's pinned hash, so signing the source would break it.
    stage = target / "macos-signing"
    stage.mkdir(parents=True, exist_ok=True)
    resources = {}
    for source, destination in ((REPO / "src-tauri/resources", "resources/"),
                                (REPO / "src-tauri/lensfun_db", "lensfun_db/"),
                                (tether_dir / "gphoto2", "gphoto2/")):
        copy = stage / source.name
        if copy.exists():
            shutil.rmtree(copy)
        shutil.copytree(source, copy)
        resources[str(copy) + "/"] = destination
        for path in sorted(copy.rglob("*")):
            if not path.is_file() or path.is_symlink():
                continue
            with path.open("rb") as binary:
                magic = binary.read(4)
            # Mach-O 32/64-bit and universal, in either byte order.
            if magic in MACHO:
                command = ["codesign", "--force", "--sign", identity, "--timestamp",
                           "--options", "runtime"]
                if keychain:
                    command.extend(["--keychain", str(keychain)])
                run([*command, str(path)])
    overlay = stage / "tauri.signing.conf.json"
    overlay.write_text(json.dumps({"bundle": {"resources": resources, "macOS": {
        "signingIdentity": identity, "hardenedRuntime": True,
        "entitlements": str(REPO / "src-tauri/DeveloperID.entitlements.plist"),
    }}}) + "\n")
    # The caller replaces the tether overlay's resource map with this map,
    # instead of merging maps (which would include the unsigned sources too).
    return overlay


def package(env, arch=None):
    signed, identity, auth = configuration(env)
    target = Path(env.get("CARGO_TARGET_DIR", REPO / "target")).resolve()
    if arch:
        # An explicit architecture always builds with --target, so its
        # artifacts never mix with the machine's own build.
        triple = f"{arch}-apple-darwin"
        tether_dir, bundle = target / triple / "tether-bundle", target / triple / "release/bundle"
        cross, helper_env = ["--target", triple], {**env, "RAWTONIC_TARGET": triple}
    else:
        arch = {"arm64": "aarch64"}.get(platform.machine(), platform.machine())
        tether_dir, bundle = target / "tether-bundle", target / "release/bundle"
        cross, helper_env = [], env
    if arch not in ARCHES:
        raise ValueError(f"Unsupported macOS architecture {arch}")
    version = json.loads((REPO / "src-tauri/tauri.conf.json").read_text())["version"]
    built_dmg = bundle / f"dmg/Rawtonic_{version}_{ARCHES[arch][1]}.dmg"
    dmg = bundle / f"dmg/Rawtonic_{version}_{ARCHES[arch][2]}.dmg"
    tether = tether_dir / "tauri.tether.conf.json"
    run(["cargo", "deny", "check", "advisories"], env=env)
    run([str(REPO / "scripts/tether/bundle_helper.sh")], env=helper_env)
    build_env = {**env, "CI": "true"}
    if not signed:
        run(["tauri", "build", *cross, "--bundles", "app,dmg", "--config", str(tether)],
            env=build_env)
        finish(bundle / "macos/Rawtonic.app", arch, built_dmg, dmg)
        return

    # Build first so the optional ONNX download and generated licenses exist.
    run(["tauri", "build", *cross, "--no-bundle", "--config", str(tether)], env=build_env)
    with tempfile.TemporaryDirectory(prefix="rawtonic-signing-") as temporary:
        keychain = None
        try:
            if env.get("APPLE_CERTIFICATE"):
                keychain, identity = import_certificate(env, Path(temporary))
            overlay = stage_resources(target, identity, keychain, tether_dir)
            config = json.loads(tether.read_text())
            signing = json.loads(overlay.read_text())["bundle"]
            config["bundle"]["resources"] = signing["resources"]
            config["bundle"]["macOS"].update(signing["macOS"])
            overlay.write_text(json.dumps(config) + "\n")
            # Tauri imports APPLE_CERTIFICATE into its own temporary keychain
            # to sign frameworks, helper, main executable, app and DMG. It also
            # notarizes/staples the app when authentication is configured.
            # For local identities Tauri forwards the name/hash verbatim to
            # codesign (no team-id extraction); keep hashes to avoid ambiguity.
            run(["tauri", "bundle", *cross, "--bundles", "app,dmg", "--config", str(overlay)],
                env={**build_env, "APPLE_SIGNING_IDENTITY": identity})
            finish(bundle / "macos/Rawtonic.app", arch, built_dmg, dmg)
            if auth:
                # Tauri 2.12.1 only notarizes the app; submit and staple the
                # final signed container too. A rejected submission exits 0.
                result = json.loads(run(["xcrun", "notarytool", "submit", str(dmg),
                                         *auth, "--wait", "--output-format", "json"], capture=True))
                if result.get("status") != "Accepted":
                    raise RuntimeError("DMG notarization was not Accepted; inspect Apple's notary log")
                run(["xcrun", "stapler", "staple", str(dmg)])
                run([str(REPO / "scripts/verify_macos_release.sh"),
                     str(bundle / "macos/Rawtonic.app"), str(dmg)])
        finally:
            if keychain:
                run(["security", "delete-keychain", str(keychain)], capture=True)


def finish(app, arch, built_dmg, dmg):
    check_architectures(app, arch)
    if built_dmg != dmg:
        built_dmg.replace(dmg)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--arch", choices=sorted(ARCHES))
    try:
        package(dict(os.environ), parser.parse_args().arch)
    except (ValueError, RuntimeError, OSError) as error:
        raise SystemExit(f"package:mac: {error}") from None
