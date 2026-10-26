#!/usr/bin/env python3
"""Packaging control-flow tests; no Apple account or native commands required."""

import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import package_macos as packaging


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
        with tempfile.NamedTemporaryFile(suffix=".p8") as key:
            signed, identity, auth = packaging.configuration({
                "APPLE_SIGNING_IDENTITY": "Developer ID Application: Example",
                **dict(zip(packaging.API, ("issuer", "key-id", key.name))),
            })
        self.assertTrue(signed)
        self.assertEqual(identity, "Developer ID Application: Example")
        self.assertEqual(auth, ["--issuer", "issuer", "--key-id", "key-id", "--key", key.name])

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
                overlay = packaging.stage_resources(target, "Developer ID Application: Example", None,
                                                    target / "tether-bundle")
            calls = [call.args[0] for call in run.call_args_list]
            self.assertEqual(len(calls), 2)
            self.assertTrue(all(call[:2] == ["codesign", "--force"] for call in calls))
            self.assertTrue(all("runtime" in call and "--timestamp" in call for call in calls))
            self.assertTrue(all("macos-signing" in call[-1] for call in calls))
            self.assertEqual(before, (runtime.read_bytes(), driver.read_bytes()))
            config = json.loads(overlay.read_text())["bundle"]
            self.assertTrue(config["macOS"]["hardenedRuntime"])
            self.assertEqual(set(config["resources"].values()), {"resources/", "lensfun_db/", "gphoto2/"})

    def signed_build(self, status="Accepted", with_auth=True, with_certificate=False):
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
               "APPLE_SIGNING_IDENTITY": "Developer ID Application: Example"}
        if with_auth:
            env.update(dict(zip(packaging.APPLE_ID, ("id", "password", "team"))))
        if with_certificate:
            env.update(APPLE_CERTIFICATE="certificate", APPLE_CERTIFICATE_PASSWORD="password")

        def command(args, **_kwargs):
            if args[:3] == ["xcrun", "notarytool", "submit"]:
                return json.dumps({"status": status})
            return None

        with patch.object(packaging, "run", side_effect=command) as run, \
                patch.object(packaging, "stage_resources", return_value=overlay), \
                patch.object(packaging, "finish"), \
                patch.object(packaging, "import_certificate", return_value=(target / "keychain", env["APPLE_SIGNING_IDENTITY"])):
            error = None
            try:
                packaging.package(env)
            except RuntimeError as caught:
                error = caught
        return [call.args[0] for call in run.call_args_list], json.loads(overlay.read_text()), error

    def test_signed_bundle_uses_only_staged_resources_then_notarizes_dmg(self):
        calls, config, error = self.signed_build()
        self.assertIsNone(error)
        self.assertEqual(config["bundle"]["resources"], {"signed": "resources/"})
        self.assertEqual(config["bundle"]["macOS"]["frameworks"], ["libgphoto2.dylib"])
        self.assertEqual(calls[2][0:3], ["tauri", "build", "--no-bundle"])
        self.assertEqual(calls[3][0:4], ["tauri", "bundle", "--bundles", "app,dmg"])
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


if __name__ == "__main__":
    unittest.main()
