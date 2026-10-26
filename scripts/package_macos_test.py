#!/usr/bin/env python3
"""Packaging control-flow tests; no Apple account or build required (rustc for the host triple)."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import package_macos as packaging


IDENTITY = "Developer ID Application: Example (TEAM123456)"
HASH = "0123456789ABCDEF0123456789ABCDEF01234567"
OTHER_HASH = "89ABCDEF0123456789ABCDEF0123456789ABCDEF"


def security_output(*identities):
    return "\n".join(f'  {index}) {sha1} "{name}"'
                     for index, (sha1, name) in enumerate(identities, 1)) + \
        f"\n     {len(identities)} valid identities found\n"


class PackagingTests(unittest.TestCase):
    def test_default_keeps_existing_commands(self):
        with patch.object(packaging, "run") as run, patch.object(packaging, "finish") as finish:
            packaging.package({})
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ["cargo", "deny", "check", "advisories"],
            [str(packaging.REPO / "scripts/tether/bundle_helper.sh")],
            ["tauri", "build", "--bundles", "app,dmg", "--config",
             str(packaging.REPO / "target/tether-bundle/tauri.tether.conf.json")],
        ])
        self.assertEqual(run.call_args.kwargs["env"], {"CI": "true"})
        app = finish.call_args.args[0]
        self.assertEqual(app, packaging.REPO / "target/release/bundle/macos/Rawtonic.app")

    def test_intel_build_has_its_own_target_folder_and_dmg_name(self):
        with patch.object(packaging, "run") as run, patch.object(packaging, "finish") as finish:
            packaging.package({}, "x86_64")
        target = packaging.REPO / "target/x86_64-apple-darwin"
        self.assertEqual(run.call_args_list[1].kwargs["env"],
                         {"RAWTONIC_TARGET": "x86_64-apple-darwin"})
        self.assertEqual(run.call_args.args[0], [
            "tauri", "build", "--target", "x86_64-apple-darwin", "--bundles", "app,dmg",
            "--config", str(target / "tether-bundle/tauri.tether.conf.json")])
        app, arch, built, dmg = finish.call_args.args
        self.assertEqual((app, arch), (target / "release/bundle/macos/Rawtonic.app", "x86_64"))
        self.assertTrue(built.name.endswith("_x64.dmg"))
        self.assertTrue(dmg.name.endswith("_x86_64.dmg"))
        self.assertEqual(built.parent, dmg.parent)

    def test_explicit_target_stages_where_the_packager_looks_also_natively(self):
        # On an Intel Mac the Intel target is the host: an explicit target
        # must still stage in target/<triple>/ (the --target build's folder).
        host = subprocess.run(["rustc", "-vV"], capture_output=True, text=True,
                              check=True).stdout.split("host: ")[1].split()[0]
        base = {key: value for key, value in os.environ.items() if key != "RAWTONIC_TARGET"}
        with tempfile.TemporaryDirectory() as target:
            def staged(**env):
                return subprocess.run(
                    [str(packaging.REPO / "scripts/tether/bundle_helper.sh"), "--print-paths"],
                    env={**base, "CARGO_TARGET_DIR": target, **env},
                    capture_output=True, text=True, check=True).stdout.strip()

            self.assertEqual(staged(RAWTONIC_TARGET=host), f"{target}/{host}/tether-bundle")
            self.assertEqual(staged(), f"{target}/tether-bundle")
            with patch.object(packaging, "run") as run, patch.object(packaging, "finish"):
                packaging.package({"CARGO_TARGET_DIR": target}, "x86_64")
            self.assertEqual(run.call_args.args[0][-1],
                             f"{Path(target).resolve()}/x86_64-apple-darwin/tether-bundle/tauri.tether.conf.json")

    def test_architecture_check_refuses_a_bundle_with_the_other_architecture(self):
        def macho(cpu):
            return b"\xcf\xfa\xed\xfe" + cpu.to_bytes(4, "little") + bytes(24)

        def fat(*cpus):
            arches = b"".join(cpu.to_bytes(4, "big") + bytes(16) for cpu in cpus)
            return b"\xca\xfe\xba\xbe" + len(cpus).to_bytes(4, "big") + arches

        intel, arm = packaging.ARCHES["x86_64"][0], packaging.ARCHES["aarch64"][0]
        with tempfile.TemporaryDirectory() as temporary:
            app = Path(temporary) / "Rawtonic.app"
            (app / "Contents/Resources/resources").mkdir(parents=True)
            (app / "Contents/MacOS").mkdir()
            (app / "Contents/MacOS/rawtonic-app").write_bytes(macho(intel))
            (app / "Contents/Resources/lens.xml").write_bytes(b"<xml/>")
            (app / "Contents/Resources/Example.class").write_bytes(b"\xca\xfe\xba\xbe\x00\x00\x00\x41")
            runtime = app / "Contents/Resources/resources/libonnxruntime.dylib"
            runtime.write_bytes(macho(intel))
            packaging.check_architectures(app, "x86_64")
            for wrong in (macho(arm), fat(intel, arm)):
                runtime.write_bytes(wrong)
                with self.subTest(wrong=wrong[:4]), self.assertRaises(RuntimeError) as error:
                    packaging.check_architectures(app, "x86_64")
                self.assertIn("libonnxruntime.dylib", str(error.exception))
            runtime.write_bytes(fat(arm))
            (app / "Contents/MacOS/rawtonic-app").write_bytes(macho(arm))
            packaging.check_architectures(app, "aarch64")
            with self.assertRaises(RuntimeError):
                packaging.check_architectures(Path(temporary) / "missing.app", "x86_64")

    def test_incomplete_or_ambiguous_credentials_fail_before_build(self):
        for env in ({"APPLE_CERTIFICATE": "certificate"},
                    {"APPLE_API_KEY": "key"},
                    dict(zip(packaging.APPLE_ID, ("id", "password", "team"))),
                    {"APPLE_SIGNING_IDENTITY": "Apple Development: Example"},
                    {"APPLE_CERTIFICATE": "cert", "APPLE_CERTIFICATE_PASSWORD": "pw",
                     "APPLE_SIGNING_IDENTITY": "-"},
                    {**dict.fromkeys(packaging.API + packaging.APPLE_ID, "value"),
                     "APPLE_SIGNING_IDENTITY": "Developer ID Application: Example"}):
            with self.subTest(keys=list(env)), patch.object(packaging, "run") as run:
                with self.assertRaises(ValueError):
                    packaging.package(env)
                run.assert_not_called()

    def test_api_credentials_use_the_explicit_private_key_path(self):
        with tempfile.NamedTemporaryFile(suffix=".p8") as key, \
                patch.object(packaging, "run", return_value=security_output((HASH, IDENTITY))):
            signed, identity, auth = packaging.configuration({
                "APPLE_SIGNING_IDENTITY": IDENTITY,
                **dict(zip(packaging.API, ("issuer", "key-id", key.name))),
            })
        self.assertTrue(signed)
        self.assertEqual(identity, IDENTITY)
        self.assertEqual(auth, ["--issuer", "issuer", "--key-id", "key-id", "--key", key.name])

    def test_local_identity_accepts_unique_name_and_exact_hash_with_duplicate_names(self):
        for requested, entries, expected in (
                (IDENTITY, ((HASH, IDENTITY),), IDENTITY),
                (HASH, ((HASH, IDENTITY), (OTHER_HASH, IDENTITY)), HASH),
                (HASH.lower(), ((HASH.lower(), IDENTITY), (OTHER_HASH, IDENTITY)), HASH)):
            with self.subTest(requested=requested), \
                    patch.object(packaging, "run", return_value=security_output(*entries)) as run:
                self.assertEqual(packaging.configuration({"APPLE_SIGNING_IDENTITY": requested}),
                                 (True, expected, []))
                run.assert_called_once_with(
                    ["security", "find-identity", "-v", "-p", "codesigning"], capture=True)

    def test_ambiguous_name_fails_before_build_and_requests_hash(self):
        with patch.object(packaging, "run", return_value=security_output(
                (HASH, IDENTITY), (OTHER_HASH, IDENTITY))) as run:
            with self.assertRaisesRegex(ValueError, "multiple valid identities.*SHA-1 hash"):
                packaging.package({"APPLE_SIGNING_IDENTITY": IDENTITY})
        self.assertEqual(len(run.call_args_list), 1)

    def test_hash_and_name_must_match_one_valid_developer_id_application(self):
        for requested, output in (
                (HASH, security_output((OTHER_HASH, IDENTITY))),
                (HASH, security_output((HASH, "Apple Development: Example"))),
                (HASH, security_output((HASH, "Developer ID Installer: Example"))),
                (HASH, security_output((HASH, IDENTITY)).replace('"\n', '" (CSSMERR_TP_CERT_EXPIRED)\n')),
                (HASH, security_output((HASH, IDENTITY), (HASH, IDENTITY))),
                (IDENTITY, security_output((HASH, IDENTITY + " extra"))),
                (HASH, "0 valid identities found")):
            with self.subTest(requested=requested, output=output), \
                    patch.object(packaging, "run", return_value=output) as run:
                with self.assertRaises(ValueError):
                    packaging.package({"APPLE_SIGNING_IDENTITY": requested})
                self.assertEqual(len(run.call_args_list), 1)

    def test_malformed_hash_is_rejected_without_querying_keychain(self):
        for requested in (HASH[:-1], HASH + "0", "Z" * 40, HASH + "\n", " " + HASH):
            with self.subTest(requested=requested), patch.object(packaging, "run") as run:
                with self.assertRaises(ValueError):
                    packaging.configuration({"APPLE_SIGNING_IDENTITY": requested})
                run.assert_not_called()

    def test_ad_hoc_identity_does_not_query_keychain(self):
        with patch.object(packaging, "run") as run:
            self.assertEqual(packaging.configuration({"APPLE_SIGNING_IDENTITY": "-"}),
                             (False, "-", []))
        run.assert_not_called()

    def test_certificate_hash_resolves_to_name_in_its_isolated_keychain(self):
        for requested in (None, IDENTITY, HASH, HASH.lower()):
            with self.subTest(requested=requested), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                env = {"APPLE_CERTIFICATE": "Y2VydA==", "APPLE_CERTIFICATE_PASSWORD": "password"}
                if requested:
                    env["APPLE_SIGNING_IDENTITY"] = requested

                def command(args, **_kwargs):
                    if args[:2] == ["security", "find-identity"]:
                        self.assertEqual(args[-1], str(directory / "signing.keychain-db"))
                        return security_output((HASH, IDENTITY))

                with patch.object(packaging, "run", side_effect=command):
                    self.assertEqual(packaging.import_certificate(env, directory),
                                     (directory / "signing.keychain-db", IDENTITY))
                self.assertFalse((directory / "certificate.p12").exists())

    def test_certificate_mismatch_or_invalid_identity_cleans_up(self):
        for output in (security_output((OTHER_HASH, IDENTITY)),
                       security_output((HASH, "Apple Development: Example")),
                       security_output((HASH, IDENTITY), (OTHER_HASH, IDENTITY))):
            with self.subTest(output=output), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)

                def command(args, **_kwargs):
                    if args[:2] == ["security", "find-identity"]:
                        return output

                with patch.object(packaging, "run", side_effect=command) as run:
                    with self.assertRaises(ValueError):
                        packaging.import_certificate({"APPLE_CERTIFICATE": "Y2VydA==",
                                                      "APPLE_CERTIFICATE_PASSWORD": "password",
                                                      "APPLE_SIGNING_IDENTITY": HASH}, directory)
                self.assertFalse((directory / "certificate.p12").exists())
                self.assertEqual(run.call_args.args[0][:2], ["security", "delete-keychain"])

    def test_command_failure_does_not_print_secret_arguments(self):
        with patch.object(packaging.subprocess, "run") as run:
            run.return_value.returncode = 1
            with self.assertRaises(RuntimeError) as error:
                packaging.run(["security", "import", "-P", "private-password"], capture=True)
        self.assertEqual(str(error.exception), "security failed (exit 1)")

    def test_failed_certificate_import_removes_temporary_keychain_and_p12(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)

            def command(args, **_kwargs):
                if args[:2] == ["security", "import"]:
                    raise RuntimeError("import failed")

            with patch.object(packaging, "run", side_effect=command) as run:
                with self.assertRaises(RuntimeError):
                    packaging.import_certificate({"APPLE_CERTIFICATE": "Y2VydA==",
                                                  "APPLE_CERTIFICATE_PASSWORD": "password"}, directory)
            self.assertFalse((directory / "certificate.p12").exists())
            self.assertEqual(run.call_args.args[0][:2], ["security", "delete-keychain"])

    def test_staged_native_resources_are_signed_without_changing_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary)
            target = repo / "target"
            sources = [repo / "src-tauri/resources", repo / "src-tauri/lensfun_db",
                       target / "tether-bundle/gphoto2/camlibs"]
            for source in sources:
                source.mkdir(parents=True)
            runtime = sources[0] / "libonnxruntime.dylib"
            driver = sources[2] / "ptp2.so"
            runtime.write_bytes(b"\xcf\xfa\xed\xfebundled-runtime")
            driver.write_bytes(b"\xca\xfe\xba\xbecamera-driver")
            (sources[0] / "license.txt").write_text("plain data")
            before = runtime.read_bytes(), driver.read_bytes()
            with patch.object(packaging, "REPO", repo), patch.object(packaging, "run") as run:
                overlay = packaging.stage_resources(target, HASH, None,
                                                    target / "tether-bundle")
            calls = [call.args[0] for call in run.call_args_list]
            self.assertEqual(len(calls), 2)
            self.assertTrue(all(call[:2] == ["codesign", "--force"] for call in calls))
            self.assertTrue(all(call[call.index("--sign") + 1] == HASH for call in calls))
            self.assertTrue(all("runtime" in call and "--timestamp" in call for call in calls))
            self.assertTrue(all("macos-signing" in call[-1] for call in calls))
            self.assertEqual(before, (runtime.read_bytes(), driver.read_bytes()))
            config = json.loads(overlay.read_text())["bundle"]
            self.assertTrue(config["macOS"]["hardenedRuntime"])
            self.assertEqual(config["macOS"]["signingIdentity"], HASH)
            self.assertEqual(set(config["resources"].values()), {"resources/", "lensfun_db/", "gphoto2/"})

    def signed_build(self, status="Accepted", with_auth=True, with_certificate=False,
                     identity=IDENTITY):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        target = Path(temporary.name)
        tether = target / "tether-bundle/tauri.tether.conf.json"
        tether.parent.mkdir()
        tether.write_text(json.dumps({"bundle": {"resources": {"unsigned": "resources/"},
                                                 "macOS": {"frameworks": ["libgphoto2.dylib"]}}}))
        overlay = target / "signing.json"
        overlay.write_text(json.dumps({"bundle": {"resources": {"signed": "resources/"},
                                                  "macOS": {"entitlements": "empty.plist"}}}))
        env = {"CARGO_TARGET_DIR": str(target),
               "APPLE_SIGNING_IDENTITY": identity}
        if with_auth:
            env.update(dict(zip(packaging.APPLE_ID, ("id", "password", "team"))))
        if with_certificate:
            env.update(APPLE_CERTIFICATE="certificate", APPLE_CERTIFICATE_PASSWORD="password")

        def command(args, **_kwargs):
            if args[:2] == ["security", "find-identity"]:
                return security_output((HASH, IDENTITY), (OTHER_HASH, IDENTITY)) if \
                    packaging.SHA1.fullmatch(identity) else security_output((HASH, IDENTITY))
            if args[:3] == ["xcrun", "notarytool", "submit"]:
                return json.dumps({"status": status})
            return None

        with patch.object(packaging, "run", side_effect=command) as run, \
                patch.object(packaging, "stage_resources", return_value=overlay) as stage, \
                patch.object(packaging, "finish"), \
                patch.object(packaging, "import_certificate", return_value=(target / "keychain", IDENTITY)):
            error = None
            try:
                packaging.package(env)
            except RuntimeError as caught:
                error = caught
            expected = IDENTITY if with_certificate else identity
            self.assertEqual(stage.call_args.args[1], expected)
            bundle_call = next(call for call in run.call_args_list
                               if call.args[0][:2] == ["tauri", "bundle"])
            self.assertEqual(bundle_call.kwargs["env"]["APPLE_SIGNING_IDENTITY"], expected)
        return [call.args[0] for call in run.call_args_list], json.loads(overlay.read_text()), error

    def test_signed_bundle_uses_only_staged_resources_then_notarizes_dmg(self):
        calls, config, error = self.signed_build()
        self.assertIsNone(error)
        self.assertEqual(config["bundle"]["resources"], {"signed": "resources/"})
        self.assertEqual(config["bundle"]["macOS"]["frameworks"], ["libgphoto2.dylib"])
        self.assertEqual(calls[3][0:3], ["tauri", "build", "--no-bundle"])
        self.assertEqual(calls[4][0:4], ["tauri", "bundle", "--bundles", "app,dmg"])
        self.assertEqual(calls[-3][:3], ["xcrun", "notarytool", "submit"])
        self.assertEqual(calls[-2][:3], ["xcrun", "stapler", "staple"])
        self.assertTrue(calls[-1][0].endswith("verify_macos_release.sh"))

    def test_rejected_notarization_never_staples_and_cleans_keychain(self):
        calls, _, error = self.signed_build(status="Invalid", with_certificate=True)
        self.assertIsNotNone(error)
        self.assertFalse(any(call[:3] == ["xcrun", "stapler", "staple"] for call in calls))
        self.assertEqual(calls[-1][:2], ["security", "delete-keychain"])

    def test_signing_without_notarization_does_not_contact_apple_notary(self):
        calls, _, error = self.signed_build(with_auth=False)
        self.assertIsNone(error)
        self.assertFalse(any(call[0] == "xcrun" for call in calls))

    def test_local_hash_reaches_staging_and_tauri_unchanged_despite_duplicate_names(self):
        _, _, error = self.signed_build(identity=HASH)
        self.assertIsNone(error)

    def test_certificate_hash_passes_resolved_name_to_tauri(self):
        _, _, error = self.signed_build(identity=HASH, with_certificate=True)
        self.assertIsNone(error)


if __name__ == "__main__":
    unittest.main()
