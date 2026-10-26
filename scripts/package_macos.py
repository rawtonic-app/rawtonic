#!/usr/bin/env python3
"""Opt-in Developer ID packaging; no credentials retains the ad-hoc path."""

import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


REPO = Path(__file__).resolve().parent.parent
API = ("APPLE_API_ISSUER", "APPLE_API_KEY", "APPLE_API_KEY_PATH")
APPLE_ID = ("APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID")


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
    if identity and identity != "-" and not identity.startswith("Developer ID Application:"):
        raise ValueError("APPLE_SIGNING_IDENTITY must be a Developer ID Application identity")
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
    return signed, identity, auth


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
        identities = run(["security", "find-identity", "-v", "-p", "codesigning",
                          str(keychain)], capture=True)
        identities = [line.split('"')[1] for line in identities.splitlines()
                      if '"Developer ID Application:' in line]
        if len(identities) != 1:
            raise ValueError("APPLE_CERTIFICATE must contain one Developer ID Application identity")
        return keychain, identities[0]
    except BaseException:
        run(["security", "delete-keychain", str(keychain)], capture=True)
        raise
    finally:
        certificate.unlink()


def stage_resources(target, identity, keychain):
    # Tauri signs externalBin and frameworks, but not Mach-O resources (.so
    # camera drivers and ONNX Runtime). Sign copies: build.rs verifies the
    # upstream runtime's pinned hash, so signing the source would break it.
    stage = target / "macos-signing"
    stage.mkdir(parents=True, exist_ok=True)
    resources = {}
    for source, destination in ((REPO / "src-tauri/resources", "resources/"),
                                (REPO / "src-tauri/lensfun_db", "lensfun_db/"),
                                (target / "tether-bundle/gphoto2", "gphoto2/")):
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
            if magic in (b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xcf",
                         b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",
                         b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca"):
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


def package(env):
    signed, identity, auth = configuration(env)
    target = Path(env.get("CARGO_TARGET_DIR", REPO / "target")).resolve()
    tether = target / "tether-bundle/tauri.tether.conf.json"
    run(["cargo", "deny", "check", "advisories"], env=env)
    run([str(REPO / "scripts/tether/bundle_helper.sh")], env=env)
    build_env = {**env, "CI": "true"}
    if not signed:
        run(["tauri", "build", "--bundles", "app,dmg", "--config", str(tether)], env=build_env)
        return

    # Build first so the optional ONNX download and generated licenses exist.
    run(["tauri", "build", "--no-bundle", "--config", str(tether)], env=build_env)
    with tempfile.TemporaryDirectory(prefix="rawtonic-signing-") as temporary:
        keychain = None
        try:
            if env.get("APPLE_CERTIFICATE"):
                keychain, certificate_identity = import_certificate(env, Path(temporary))
                if identity and identity != certificate_identity:
                    raise ValueError("APPLE_SIGNING_IDENTITY does not match APPLE_CERTIFICATE")
                identity = certificate_identity
            overlay = stage_resources(target, identity, keychain)
            config = json.loads(tether.read_text())
            signing = json.loads(overlay.read_text())["bundle"]
            config["bundle"]["resources"] = signing["resources"]
            config["bundle"]["macOS"].update(signing["macOS"])
            overlay.write_text(json.dumps(config) + "\n")
            # Tauri imports APPLE_CERTIFICATE into its own temporary keychain
            # to sign frameworks, helper, main executable, app and DMG. It also
            # notarizes/staples the app when authentication is configured.
            run(["tauri", "bundle", "--bundles", "app,dmg", "--config", str(overlay)],
                env={**build_env, "APPLE_SIGNING_IDENTITY": identity})
            if auth:
                version = json.loads((REPO / "src-tauri/tauri.conf.json").read_text())["version"]
                arch = run(["uname", "-m"], capture=True).strip()
                arch = {"arm64": "aarch64", "x86_64": "x64"}[arch]
                dmg = target / f"release/bundle/dmg/Rawtonic_{version}_{arch}.dmg"
                # Tauri 2.12.1 only notarizes the app; submit and staple the
                # final signed container too. A rejected submission exits 0.
                result = json.loads(run(["xcrun", "notarytool", "submit", str(dmg),
                                         *auth, "--wait", "--output-format", "json"], capture=True))
                if result.get("status") != "Accepted":
                    raise RuntimeError("DMG notarization was not Accepted; inspect Apple's notary log")
                run(["xcrun", "stapler", "staple", str(dmg)])
                run([str(REPO / "scripts/verify_macos_release.sh"),
                     str(target / "release/bundle/macos/Rawtonic.app"), str(dmg)])
        finally:
            if keychain:
                run(["security", "delete-keychain", str(keychain)], capture=True)


if __name__ == "__main__":
    try:
        package(dict(os.environ))
    except (ValueError, RuntimeError, OSError) as error:
        raise SystemExit(f"package:mac: {error}") from None
