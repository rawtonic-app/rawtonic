# Releasing

A release is one command, a read of its summary, and a second command on the user's word:

```sh
scripts/release.sh --dry-run 0.1.0-alpha.12   # the plan: stages and commands, no side effects
scripts/release.sh 0.1.0-alpha.12             # bump PR → builds → smokes → notes → website branch → DRAFT prerelease
scripts/release.sh --publish 0.1.0-alpha.12   # only on the user's word: publish, website merged and deployed, verified
```

The release run never publishes a release, pushes a tag or deploys the website: it stops at a draft prerelease (`gh release create --draft --prerelease`) and a website branch. Read the summary table, the generated notes and the website branch, edit the draft on GitHub where needed; `--publish` follows only on the user's word. Do not publish while parity or benchmark gates are open (AGENTS.md).

Before running it, write the release's section under `## Unreleased` in `CHANGELOG.md` and merge it: the bump turns it into `## VERSION — DATE`, and the section is bundled into the app (What's new), so it must be final before the build. Use the publish day as the date (`--date`). In the same task write the website's What's new: in the website clone (`RAWTONIC_WEBSITE_DIR`), on a branch `release-VERSION` from its `origin/main`, `release.json` `whatsNew` = `{"version": VERSION, "en": {…}, "de": {…}}`, each language with a one-line `summary`, three `items` (`title`, `text`, plain text) and a `note` (how to update; what changes for existing edits), from the CHANGELOG section; German as on the rest of the site (du, the app's German labels). Also read every sentence on the site that names the previous release: `node scripts/build.mjs --strict` refuses an older version outside an element marked `data-history` (website README, Release). Commit on that branch (push it if the release runs on another machine) and switch the clone back to `main` (`git switch main`): the `site` stage checks the branch out in its own worktree, which Git refuses while the clone has it. `preflight` refuses a run, before the builds, while the branch is missing, checked out elsewhere, or without this version's `whatsNew`; the `site` stage only adds the facts and checks.

## What it does

| Stage | What | Where |
|---|---|---|
| `preflight` | tools, `gh` login, credentials file, photographs, recipes, the website branch `release-VERSION` (exists, not checked out elsewhere, `whatsNew` of VERSION), Linux host, disk; finds the previous release | this Mac |
| `bump` | worktree on `release-VERSION` from `origin/main`: version in `package.json`, `package-lock.json` (top entries), `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `Cargo.lock`, both `THIRD_PARTY_NOTICES.md` rows; CHANGELOG heading; `scripts/ship.sh` (check, second-model review, merge) | this Mac |
| `source` | detached worktrees of the merged commit for the release build and the QA build, `npm ci` | this Mac |
| `prev` | the previous release's DMGs, AppImage and LGPL tarballs from GitHub; its apps copied out of the read-only mounted DMGs | this Mac |
| `mac-arm64`, `mac-x86_64` | signed and notarized DMGs (`npm run package:mac` / `package:mac:intel` with the credentials sourced in a subshell), one after the other in one worktree (they cannot share it at once; one cargo build per host); the same notarized `Rawtonic.app` as in-app update package `Rawtonic_VERSION_ARCH.app.tar.gz` | this Mac |
| `dmg-arm64`, `dmg-x86_64` | browser quarantine flag on a copy of the DMG; Gatekeeper (`spctl`) and stapler on DMG and copied app ("Notarized Developer ID"), deep strict codesign, architecture, relay address, version; headless JPEG exports of the photographs (HOME in scratch) byte-compared with the previous release's app (Intel also against Apple Silicon, informational: X-Trans differs slightly) | this Mac |
| `ident` | saved-recipe identity: each photograph × each recipe as 16-bit TIFF from the previous and the new Apple Silicon app; a difference is re-run with fresh HOMEs before it counts | this Mac |
| `qa-app` | a `qa-control` build of the same commit with a QA identifier (never `cc.rawtonic.editor`), own worktree and `CARGO_TARGET_DIR`, plus the `rawtonic` CLI | this Mac |
| `gui` | `scripts/release/gui_smoke.py`, below | this Mac |
| `relay` | an optimized build of `rawtonic-contribute` sends one report to the build's relay and deletes it | this Mac, network |
| `linux`, `linux-smoke` | `git archive` of the commit to the Linux host, `packaging/linux/build-appimage.sh` under the host's build lock; relay address, update information, host exports (`xvfb-run`) byte-compared with the previous AppImage, exports in fresh Ubuntu 22.04/24.04, Debian 12 and Fedora 42 containers (`scripts/release/linux.sh`) | Linux host over SSH |
| `windows` | pushes `release-build/VERSION` at the commit (the workflow takes branches, not SHAs), `gh workflow run windows-installer.yml`, waits, requires a green run whose log shows the WARP refusal (or an export) and `uninstaller exit 0`, downloads and checks the installer, relay address in the unpacked `rawtonic-app.exe` (7z on the Linux host), keeps the executable's symbols privately (`RAWTONIC_SYMBOLS_REPO`); deletes the branch | GitHub runner |
| `updater` | the in-app update: both `.app.tar.gz` unpacked and checked (deep strict codesign, stapler, Gatekeeper "Notarized Developer ID", version); the two `.app.tar.gz`, the AppImage and the Windows per-user setup signed with the updater key (`tauri signer sign --app-version VERSION`, key and password sourced in a subshell); `latest.json` written (`scripts/release/updater_manifest.py make`: version, a note with the release URL, date, per package key `darwin-aarch64-app`, `darwin-x86_64-app`, `linux-x86_64-appimage`, `windows-x86_64-nsis` the URL among this release's files and the signature) and verified with the public key the release commit's `tauri.conf.json` carries | this Mac |
| `creds` | every signing credential value (Apple credentials, the updater key's password and the key itself; not the identity hash, which codesign prints and is public) counted in all logs, the apps, the notes and `latest.json`: must be 0; `gitleaks dir` on the logs | this Mac |
| `sums` | LGPL source tarballs (from the previous release, else the pinned URL) checked against the pins in `scripts/tether/build_libgphoto2.sh`; `SHA256SUMS.txt` | this Mac |
| `notes` | `scripts/release/notes.py`: intro, Highlights, Fixes, Changes to existing edits and Known gaps from the CHANGELOG section; warning, Install, camera-library sources and Feedback from the previous release's notes with the version swapped; the SHA-256 list | this Mac |
| `site` | the website's release facts on its branch `release-VERSION` (a worktree `WORK/website` of `RAWTONIC_WEBSITE_DIR`; the local branch, else `origin/release-VERSION`, else a new one from `origin/main`): `scripts/release/website.py write` sets `release.json`'s version, date and `dateText` (from the CHANGELOG heading), tag, the four downloads (`mac` arm64 DMG, `macIntel`, `linux` AppImage, `windows` MSI) and `otherAssets` (every other file of `SHA256SUMS.txt`, and that file) with size (MiB, rounded half-up) and SHA-256 from `SHA256SUMS.txt`; fails unless `whatsNew` is this version's (en and de); `node scripts/build.mjs --strict` with `RAWTONIC_CHANGELOG` = the release's CHANGELOG (the tag does not exist yet) must pass; commits `release: VERSION` on the branch. Never pushes or deploys | this Mac |
| `draft` | `gh release create vVERSION --draft --prerelease --target SHA` with every asset (DMGs, `.app.tar.gz`, AppImage + `.zsync`, Windows installers + `SHA256SUMS-windows.txt`, `latest.json`, LGPL tarballs, `SHA256SUMS.txt`); only when every other stage passed; then every URL in `latest.json` must name one of the draft's assets | GitHub |

After `prev`, three chains run in parallel: the Mac chain (Apple Silicon build → its smoke and the identity check beside the Intel build → Intel smoke → QA build → GUI smoke → relay), the Linux chain, and the Windows workflow; `updater`, `creds`, `sums`, `notes`, `site` and `draft` follow. Each stage logs to `WORK/logs/STAGE.log` (mode 600) and leaves `WORK/state/STAGE.ok`; the run ends with a table of every stage, its result, time and log.

## Publishing

`scripts/release.sh --publish VERSION` (only on the user's word) runs three stages in the same work folder, resumable like the others (`--from`, `--only` with `--publish`):

| Stage | What |
|---|---|
| `publish` | the draft must target the built commit, a tag `vVERSION` already on origin must name that commit, and the release's files must be exactly the tested ones (names, and GitHub's SHA-256 digest of each against `SHA256SUMS.txt`: a file replaced on GitHub during review stops the publish); `gh api -X PATCH …/releases/ID -f tag_name=vVERSION -f target_commitish=SHA -F draft=false -F prerelease=true` publishes it and GitHub creates the tag at that commit; then the release is no draft and the tag on origin is the commit. An already published release is only checked |
| `site-deploy` | in `WORK/website`: unless `release-VERSION` is already in the website's `origin/main`, a `--no-ff` merge into it (`Merge release-VERSION: VERSION`), `node scripts/build.mjs --strict` (release history now from the tag), `git push origin HEAD:main`; then the website's `scripts/deploy.sh` (its tests, the strict build, `wrangler deploy`, 200 check) |
| `site-verify` | `https://rawtonic.cc/` and `/de/` fetched with a cache-busting query (retried for 5 minutes) must link the four downloads of VERSION, say `Rawtonic VERSION` and carry the What's new heading `New in alpha.N.` / `Neu in alpha.N.` (`website.py verify-page`); the release's `latest.json` must download and name VERSION; the four downloads must answer; then the website worktree and its merged branch are removed |

Announcements stay by hand.

## Reruns

A full run takes about an hour, most of it the Windows runner: start it where it survives a closed terminal (`tmux`, `nohup … &`). An interrupted run keeps its markers; a step already running on the Linux host finishes there, and the rerun waits for it (build lock, smoke lock); a remembered Windows run is waited for again. Only one run per version at a time (`WORK/run.lock`).

A rerun with the same version skips every stage that passed. `--from STAGE` redoes that stage and all after it, `--only STAGE` runs one stage (its inputs must exist), e.g. `--only gui` after fixing the QA machine. A failed Windows run is not waited for again: the rerun starts a new one.

Other options: `--no-bump` builds `--sha` (default `origin/main`), which must already carry the version and its CHANGELOG section (a hotfix branch, or a rebuild); `--no-draft` stops before `gh release create`; `--prev VERSION` sets the release the identity checks compare with; `--accept-ident-diff` lists identity differences instead of failing — only for an export change the CHANGELOG states.

A work folder belongs to one commit. A change after the bump means rebuilding every asset: move the work folder aside (or set another `RAWTONIC_RELEASE_WORK`) and run again (`--no-bump --sha FIXED` when the version is already merged); the script refuses to mix commits in one folder. Linux host paths may hold only letters, digits and `. _ / -`.

## Configuration

Machine-specific values come from the environment or from `~/.config/rawtonic/release.env` (another file: `RAWTONIC_RELEASE_ENV`), which is sourced by bash and never committed. Keep credentials only in the signing file it points at.

| Variable | Meaning |
|---|---|
| `RAWTONIC_SYMBOLS_REPO` | a **private** GitHub repository (`owner/name`) where the `windows` stage keeps each release's `rawtonic-app.exe` and `rawtonic_app.pdb` as release `symbols-vVERSION` (it refuses a public one); unset: the zip stays only in the work folder (DEVELOPMENT.md, Symbolizing a Windows crash report) |
| `RAWTONIC_SIGNING_ENV` | file exporting `APPLE_SIGNING_IDENTITY`, `APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH` (or the Apple ID trio) and the updater key's `TAURI_SIGNING_PRIVATE_KEY_PATH` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (below); default `~/.config/rawtonic/signing.env` (DEVELOPMENT.md, macOS release signing) |
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
| `RAWTONIC_WEBSITE_DIR` | the clone of the website repository (rawtonic-app/website) the `site` stage and `--publish` work in, through a worktree; `preflight` refuses a run without it. `--publish` deploys from it, so `npx wrangler login` must have been done once on this Mac |

The Linux host needs Docker, `flock`, `xvfb-run` and `7z`; this Mac needs Xcode command line tools, Rust with the `x86_64-apple-darwin` target for both toolchains (DEVELOPMENT.md, Intel Macs), Node, `gh` (logged in), optional `gitleaks`.

## The updater key

The in-app update ("Install update" in the update notice) installs only packages signed with Rawtonic's own updater key, an Ed25519 minisign key made with `npx tauri signer generate`. The app carries its public half (`plugins.updater.pubkey` in `src-tauri/tauri.conf.json`, with `requireSignedVersion`: a signature names the version it was made for). The private key lives only on the release Mac, in `~/.config/rawtonic/updater.key` (mode 600; `updater.key.pub` beside it), with its password in the signing file — never in git, never on CI: the Windows and Linux packages are signed here after they are downloaded from their builds. `preflight` refuses a run whose key file is missing, not mode 600, or does not match the public key in the commit being released.

- **Back up** `updater.key` and the signing file together (an encrypted store the maintainer controls). Without them no further in-app update can be signed for the copies already installed.
- **Rotation** (a lost or leaked key): make a new key, put its public half into `tauri.conf.json` and release. Copies that carry the old public key refuse packages signed with the new one, so their users update once by hand (the notice's browser link still works); every copy from that release on trusts the new key. A leaked key also needs an announcement: until users have updated by hand, an attacker who controls their connection to GitHub could offer them a package signed with it.

## Testing the in-app update

`docs/testing/in-app-update.md` describes the end-to-end test with two QA builds and a local feed (`scripts/qa/update_feed.py`), signed with a throwaway QA key; release builds never read that feed.

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

- Announcements stay by hand.
- The Linux desktop GUI (KDE focus rules defeat scripted input) and a Windows run on a real GPU: the Windows runner has none, so the notes say the build was smoke-tested without a graphics card.
- Real Intel hardware: the Intel DMG is smoke-tested under Rosetta only.
