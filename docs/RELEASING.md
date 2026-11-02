# Releasing

A release is one command, then a read of its summary and a publish by hand:

```sh
scripts/release.sh --dry-run 0.1.0-alpha.12   # the plan: stages and commands, no side effects
scripts/release.sh 0.1.0-alpha.12             # bump PR → builds → smokes → notes → DRAFT prerelease
```

The script never publishes a release and never pushes a tag: it stops at a draft prerelease (`gh release create --draft --prerelease`). Read the summary table and the generated notes, edit the draft on GitHub where needed, then publish it by hand. Do not publish while parity or benchmark gates are open (AGENTS.md).

Before running it, write the release's section under `## Unreleased` in `CHANGELOG.md` and merge it: the bump turns it into `## VERSION — DATE`, and the section is bundled into the app (What's new), so it must be final before the build. Use the publish day as the date (`--date`).

## What it does

| Stage | What | Where |
|---|---|---|
| `preflight` | tools, `gh` login, credentials file, photographs, recipes, Linux host, disk; finds the previous release | this Mac |
| `bump` | worktree on `release-VERSION` from `origin/main`: version in `package.json`, `package-lock.json` (top entries), `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `Cargo.lock`, both `THIRD_PARTY_NOTICES.md` rows; CHANGELOG heading; `scripts/ship.sh` (check, second-model review, merge) | this Mac |
| `source` | detached worktrees of the merged commit for the release build and the QA build, `npm ci` | this Mac |
| `prev` | the previous release's DMGs, AppImage and LGPL tarballs from GitHub; its apps copied out of the read-only mounted DMGs | this Mac |
| `mac-arm64`, `mac-x86_64` | signed and notarized DMGs (`npm run package:mac` / `package:mac:intel` with the credentials sourced in a subshell), one after the other in one worktree (they cannot share it at once; one cargo build per host) | this Mac |
| `dmg-arm64`, `dmg-x86_64` | browser quarantine flag on a copy of the DMG; Gatekeeper (`spctl`) and stapler on DMG and copied app ("Notarized Developer ID"), deep strict codesign, architecture, relay address, version; headless JPEG exports of the photographs (HOME in scratch) byte-compared with the previous release's app (Intel also against Apple Silicon, informational: X-Trans differs slightly) | this Mac |
| `ident` | saved-recipe identity: each photograph × each recipe as 16-bit TIFF from the previous and the new Apple Silicon app; a difference is re-run with fresh HOMEs before it counts | this Mac |
| `qa-app` | a `qa-control` build of the same commit with a QA identifier (never `cc.rawtonic.editor`), own worktree and `CARGO_TARGET_DIR`, plus the `rawtonic` CLI | this Mac |
| `gui` | `scripts/release/gui_smoke.py`, below | this Mac |
| `relay` | an optimized build of `rawtonic-contribute` sends one report to the build's relay and deletes it | this Mac, network |
| `linux`, `linux-smoke` | `git archive` of the commit to the Linux host, `packaging/linux/build-appimage.sh` under the host's build lock; relay address, update information, host exports (`xvfb-run`) byte-compared with the previous AppImage, exports in fresh Ubuntu 22.04/24.04, Debian 12 and Fedora 42 containers (`scripts/release/linux.sh`) | Linux host over SSH |
| `windows` | pushes `release-build/VERSION` at the commit (the workflow takes branches, not SHAs), `gh workflow run windows-installer.yml`, waits, requires a green run whose log shows the WARP refusal (or an export) and `uninstaller exit 0`, downloads and checks the installer, relay address in the unpacked `rawtonic-app.exe` (7z on the Linux host); deletes the branch | GitHub runner |
| `creds` | every signing credential value (not the identity hash, which codesign prints and is public) counted in all logs, the apps and the notes: must be 0; `gitleaks dir` on the logs | this Mac |
| `sums` | LGPL source tarballs (from the previous release, else the pinned URL) checked against the pins in `scripts/tether/build_libgphoto2.sh`; `SHA256SUMS.txt` | this Mac |
| `notes` | `scripts/release/notes.py`: intro, Highlights, Changes to existing edits and Known gaps from the CHANGELOG section; warning, Install, camera-library sources and Feedback from the previous release's notes with the version swapped; the SHA-256 list | this Mac |
| `draft` | `gh release create vVERSION --draft --prerelease --target SHA` with every asset (DMGs, AppImage + `.zsync`, Windows installer + `SHA256SUMS-windows.txt`, LGPL tarballs, `SHA256SUMS.txt`); only when every other stage passed | GitHub |

After `prev`, three chains run in parallel: the Mac chain (Apple Silicon build → its smoke and the identity check beside the Intel build → Intel smoke → QA build → GUI smoke → relay), the Linux chain, and the Windows workflow. Each stage logs to `WORK/logs/STAGE.log` (mode 600) and leaves `WORK/state/STAGE.ok`; the run ends with a table of every stage, its result, time and log.

## Reruns

A full run takes about an hour, most of it the Windows runner: start it where it survives a closed terminal (`tmux`, `nohup … &`). An interrupted run keeps its markers; a step already running on the Linux host finishes there, and the rerun waits for it (build lock, smoke lock); a remembered Windows run is waited for again. Only one run per version at a time (`WORK/run.lock`).

A rerun with the same version skips every stage that passed. `--from STAGE` redoes that stage and all after it, `--only STAGE` runs one stage (its inputs must exist), e.g. `--only gui` after fixing the QA machine. A failed Windows run is not waited for again: the rerun starts a new one.

Other options: `--no-bump` builds `--sha` (default `origin/main`), which must already carry the version and its CHANGELOG section (a hotfix branch, or a rebuild); `--no-draft` stops before `gh release create`; `--prev VERSION` sets the release the identity checks compare with; `--accept-ident-diff` lists identity differences instead of failing — only for an export change the CHANGELOG states.

A change after the bump means rebuilding every asset: use a new work folder (move the old one aside) or `--from source` after the fix is merged.

## Configuration

Machine-specific values come from the environment or from `~/.config/rawtonic/release.env` (another file: `RAWTONIC_RELEASE_ENV`), which is sourced by bash and never committed. Keep credentials only in the signing file it points at.

| Variable | Meaning |
|---|---|
| `RAWTONIC_SIGNING_ENV` | file exporting `APPLE_SIGNING_IDENTITY`, `APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH` (or the Apple ID trio); default `~/.config/rawtonic/signing.env` (DEVELOPMENT.md, macOS release signing) |
| `RAWTONIC_RELEASE_WORK` | work folder; each version gets `WORK/VERSION` (worktrees, assets, logs, state); default `~/rawtonic-release-work` |
| `RAWTONIC_BUILD_LOCK` | command prefix for heavy builds on this Mac (a shared build lock), optional |
| `RAWTONIC_RELEASE_PHOTOS` | folder of RAW test photographs (copies; only read) |
| `RAWTONIC_RELEASE_PHOTOS_SUMS` | their `shasum -a 256` list, checked before and after; default `PHOTOS/SHA256SUMS` |
| `RAWTONIC_IDENT_RECIPES` | folder of saved recipes (`*.json`, as `export --adjustments` takes them) for the identity check |
| `RAWTONIC_GUI_RAW` | the RAW the GUI smoke copies into its session; default the first CR3 among the photographs |
| `RAWTONIC_QA_IDENTIFIER`, `RAWTONIC_QA_PORT` | QA app identifier (default `cc.rawtonic.qa.release`) and QA control port (default 7989; check it is free) |
| `RAWTONIC_LINUX_SSH` | the ssh command for the Linux build host, e.g. `ssh -i KEY -o BatchMode=yes user@host` |
| `RAWTONIC_LINUX_DIR` | folder on the host (relative to its home or absolute) for sources and smokes; default `rawtonic-release` |
| `RAWTONIC_LINUX_CACHE`, `RAWTONIC_LINUX_CACHE_SEED` | the cargo/npm cache for `build-appimage.sh`, first copied from the seed (an earlier release's cache) when missing |
| `RAWTONIC_LINUX_LOCK` | the host's build lock file (`flock`), optional |
| `RAWTONIC_LINUX_PHOTOS` | folder of photographs with a `SHA256SUMS` on the host (`packaging/linux/smoke-test.sh`) |
| `RAWTONIC_REPO` | GitHub repository; default `rawtonic-app/rawtonic` |

The Linux host needs Docker, `flock`, `xvfb-run` and `7z`; this Mac needs Xcode command line tools, Rust with the `x86_64-apple-darwin` target for both toolchains (DEVELOPMENT.md, Intel Macs), Node, `gh` (logged in), optional `gitleaks`.

## The GUI smoke

`scripts/release/gui_smoke.py` drives a QA build through the QA control channel (docs/testing/qa-control.md) instead of a person clicking. It runs as the `gui` stage or alone:

```sh
python3 scripts/release/gui_smoke.py --app "path/Rawtonic QA release.app" --cli target/release/rawtonic \
  --raw photo.CR3 --version 0.1.0-alpha.12 --work /path/to/scratch [--port 7989]
```

In a new folder it makes a fresh profile (HOME) and a session with a copy of the RAW (the CLI's `create` and `ingest`), then: first start on the empty profile and a clean quit; a start as after an update (the profile's `lastRunVersion` set to an older version), where What's new must name the version; the session opens, a double click opens the photo in Develop, and a capture of the window must show a picture in the viewer (not one flat colour); one export into the session's Output folder must be a JPEG; quit through macOS (an Apple Event, as the Dock does) with exit 0, no panic in the app log, the RAW unchanged. The app takes the keyboard focus for about a minute. It refuses the user's identifier `cc.rawtonic.editor`.

## After the release

Check `README.md` against the new CHANGELOG section (features, What it does, the keyboard table, limitations, download file names) and fix it in a PR; the summary's last line reminds of it.

## Not covered

- Publishing, the tag, the website and announcements stay by hand.
- The Linux desktop GUI (KDE focus rules defeat scripted input) and a Windows run on a real GPU: the Windows runner has none, so the notes say the build was smoke-tested without a graphics card.
- Real Intel hardware: the Intel DMG is smoke-tested under Rosetta only.
