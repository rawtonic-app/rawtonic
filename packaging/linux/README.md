# Linux packages beyond the AppImage

Both packages repackage the release AppImage (`Rawtonic_<version>_amd64.AppImage` from the GitHub release, pinned by SHA-256). They add no build of their own, so what they ship is exactly the tested release (built by `build-appimage.sh`, glibc ≤ 2.35, with ONNX Runtime, the WebGPU plugin and the tether helper bundled). Neither is published: Flathub and the AUR need the maintainer's accounts (below).

## Arch Linux — `arch/PKGBUILD` (`rawtonic-bin`)

A `-bin` package from the AppImage, not a source package: a source build needs Rust 1.98, Node 24, the pinned libgphoto2/libusb build of the tether helper and the pinned ONNX Runtime download, none of which pacman can provide, and it would produce a different binary than the one the release tests qualify. AUR users know `-bin` packages for this case. The AppImage's squashfs is unpacked at build time (no FUSE, nothing of the app runs) and installed as an AppDir in `/opt/rawtonic`; `/usr/bin/rawtonic` starts its `AppRun` (bundled WebKitGTK 2.50, GTK 3, GStreamer) and hands it the folder the command ran in as `OWD`. Only what linuxdeploy leaves out of every AppImage and the GPU stack are dependencies. Because the AppDir carries its own WebKitGTK, the NVIDIA DMA-BUF handling of PR #99 applies unchanged.

`pkgver` is `0.1.0alpha.19` for the release `v0.1.0-alpha.19` (pacman versions allow no hyphen; `vercmp` sorts `0.1.0alpha.19` before `0.1.0` and after `0.1.0alpha.17.1`, checked by the test). Per release: change `pkgver`, `_tag` and the checksum from `SHA256SUMS.txt`.

Check (needs Docker): `packaging/linux/arch/test-in-container.sh PHOTOS OUT [CACHE]` — in a fresh `archlinux` container: `vercmp`, `makepkg -s` (declared dependencies only), `namcap`, `pacman -U`, `ldd` over every bundled library with only the declared dependencies installed, then (with Xvfb and Mesa's software Vulkan as a stand-in for a desktop) a headless JPEG export of one RAW, a relative-path export, a 30 s GUI start, `pacman -Rns` and a check that nothing of the package is left.

## Flatpak — `flatpak/cc.rawtonic.editor.yml`

`org.freedesktop.Platform` 24.08 with the AppImage's AppDir in `/app/rawtonic` (the same launcher as the Arch package). The AppDir brings its own WebKitGTK, so the runtime supplies glibc, Mesa/Vulkan and the NVIDIA GL extension mechanism only. Build and run: `flatpak-builder --user --install-deps-from=flathub --force-clean --install build-dir cc.rawtonic.editor.yml`, `flatpak run cc.rawtonic.editor`. Check (needs Docker and network for Flathub): `packaging/linux/flatpak/test-in-container.sh PHOTOS OUT [CACHE]` — builds and installs it with `flatpak-builder` in an Arch container, exports one RAW headless through the sandbox, starts the GUI for 30 s under Xvfb and uninstalls.

Permissions: Wayland, X11 fallback, shared IPC, DRI (GPU); `xdg-pictures` and `/run/media` (cards) beside the file chooser portal, which grants any other folder the user picks; network for what the user starts (AI model downloads, problem reports through the relay, links in the browser — nothing else, PRIVACY.md).

What does not work or differs inside the Flatpak:

- **Updates:** the app never contacts GitHub inside a Flatpak (it recognises `FLATPAK_ID`, `/.flatpak-info`): no startup check, and "Check now" reports a failed check. Updates come from `flatpak update cc.rawtonic.editor`. A Preferences text saying so is open.
- **Tethering over USB:** the manifest grants no USB access, so `rawtonic-tether` cannot reach a camera. Not tried with `--device=all`.
- **NVIDIA:** needs the `org.freedesktop.Platform.GL.nvidia-*` extension matching the host driver; not tried (the check ran with software Vulkan).
- Folders outside Pictures and the portal's grants (for example an external disk mounted elsewhere) must be granted with `flatpak override --user --filesystem=PATH cc.rawtonic.editor`.
- The AppDir's old Ubuntu libraries make the Flatpak larger (about 440 MB installed) than a runtime-WebKit build; Flathub itself wants a build from source (offline Cargo/npm sources, tether and ONNX Runtime pinned downloads), which is not done.

## Debian package

Not built (user decision, 2026-10-15: AppImage only; Tauri's `.deb` is built but not qualified or shipped).

## Open: needs the maintainer's accounts

- AUR: an account, an SSH key registered there, then `git push` of `PKGBUILD` and `.SRCINFO` (`makepkg --printsrcinfo`) to `ssh://aur@aur.archlinux.org/rawtonic-bin.git`.
- Flathub: a pull request to `flathub/flathub` for `cc.rawtonic.editor` (new-app review; wants a source build, see above) or a self-hosted Flatpak repository; the `cc.rawtonic.editor` ID is tied to the `rawtonic.cc` domain.
- CI job for these checks: not added (it needs a published release asset and about 10 minutes of Docker time per package).
