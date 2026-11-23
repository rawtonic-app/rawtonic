# Testing the in-app update end to end

"Install update" (update notice) downloads a release's package, checks its signature and installs it on "Restart now" (`src-tauri/src/update_check/install.rs`, PRIVACY.md). Unit tests cover the release selection, the install kinds, the fallbacks and the notice's states; this test runs the real thing on one machine: QA build **N** updates itself to QA build **N+1** from a local feed. It never touches the user's `cc.rawtonic.editor` app, its settings or the release key.

Two things exist only for this test, and only in QA builds:

- `RAWTONIC_QA_UPDATE_FEED=http://HOST:PORT` replaces GitHub for the check and the update. The app reads it only when its identifier starts with `cc.rawtonic.qa`.
- The QA build's `--config` sets a throwaway public key and `dangerousInsecureTransportProtocol` (the feed is plain http). Release builds have neither.

## 1. A throwaway QA key

```sh
S=~/rawtonic-scratch/upd-e2e; mkdir -p $S
npx tauri signer generate --ci -w $S/qa-updater.key -p qa-only-password
QA_PUB=$(cat $S/qa-updater.key.pub)
```

## 2. Two QA builds

Build N and N+1 from the same commit; only the version differs (any two alpha versions above the release you test from, e.g. `0.1.0-alpha.90` and `0.1.0-alpha.91`). In a worktree with `npm ci` done, with its own `CARGO_TARGET_DIR`:

```sh
qa_build() {  # VERSION BUNDLE
  npm run tauri -- build --bundles "$2" --config "{\"identifier\":\"cc.rawtonic.qa.update\",\"productName\":\"Rawtonic QA update\",\"version\":\"$1\",\"plugins\":{\"updater\":{\"pubkey\":\"$QA_PUB\",\"dangerousInsecureTransportProtocol\":true}}}"
}
```

| Platform | Bundle | Package to serve for N+1 | Package key |
|---|---|---|---|
| Linux | `appimage` | `bundle/appimage/Rawtonic QA update_N+1_amd64.AppImage` (rename it without spaces, e.g. `Rawtonic_QA_update_N+1_amd64.AppImage`; keep the `amd64` / `x86_64` part: the notice picks the installer by that architecture in the name and without it offers no Install update) | `linux-x86_64-appimage` |
| Windows | `nsis` | `bundle\nsis\Rawtonic QA update_N+1_x64-setup.exe` (rename it without spaces; keep `x64-setup.exe`) | `windows-x86_64-nsis` |
| macOS | `app,dmg` | `COPYFILE_DISABLE=1 tar --no-mac-metadata -czf qa_N+1_aarch64.app.tar.gz -C bundle/macos "Rawtonic QA update.app"`, plus the DMG (renamed `qa_N+1_aarch64.dmg`) as `--installer`: the app offers Install update only when the release has an installer for the computer | `darwin-aarch64-app` |

Copy N's package aside after the first build: the second build overwrites the bundle folder.

**Windows: only on a clean test machine or VM** (or a Windows account that has never had Rawtonic installed). The QA setup changes the name and identifier but reuses the release installer's hooks, which look for the release install folder and write the release's registry values: on an account with Rawtonic installed it could install over that copy.

## 3. Sign N+1 and serve it

```sh
TAURI_SIGNING_PRIVATE_KEY_PATH=$S/qa-updater.key TAURI_SIGNING_PRIVATE_KEY_PASSWORD=qa-only-password \
  npx tauri signer sign --app-version 0.1.0-alpha.91 $S/feed/PACKAGE      # writes PACKAGE.sig
python3 scripts/qa/update_feed.py --version 0.1.0-alpha.91 --dir $S/feed \
  --package linux-x86_64-appimage=PACKAGE --port 8765                    # leave it running; it logs every request
# macOS: --package darwin-aarch64-app=qa_N+1_aarch64.app.tar.gz --installer qa_N+1_aarch64.dmg
```

## 4. Install N and update

Install or place N as a user would (AppImage anywhere writable; the Windows setup for your account; the macOS app in `~/Applications`), start it with the feed (`RAWTONIC_QA_UPDATE_FEED=http://127.0.0.1:8765`; macOS: `open --env RAWTONIC_QA_UPDATE_FEED=… "Rawtonic QA update.app"`), open a scratch session with a copy of a RAW, then check for updates: macOS app menu › Check for Updates…, other systems Settings › General › Check now (the startup check runs at most once per 24 hours, so a second launch of the same profile does not ask the feed again; a change to the feed is only seen after a manual check).

| Case | Do | Expect |
|---|---|---|
| Happy path | Install update → wait → Restart now | progress with percent and MB; "ready to install"; the app restarts and About shows N+1; the session, its edits and the RAW unchanged (SHA-256 before/after); the feed log shows `latest.json` and the package fetched once each, **nothing before the click** |
| Photo without a library | Start N with a scratch RAW copy as its only argument, edit it, then click Restart now. On Linux, repeat from the photo's directory with the AppImage and a relative filename containing a space (e.g. `"qa one.NEF"`). On macOS or Linux, also launch with a photo argument, open a library before restarting, and repeat. | When the photo is open on its own, N+1 reopens it in Develop with the edit. The restarted AppImage uses the original launch directory. With the library open, the restart note records no photo and the library reopens without opening the photo from the original command line (covered by `restart_reopen::tests`). In each case the scratch RAW's SHA-256 is unchanged and `reopen-after-restart.json` is consumed. |
| Nothing before the click | start N, dismiss nothing, wait a minute | the feed log shows only `/releases.json` |
| Edits are saved | change a slider, Restart now at once | after the restart the change is there |
| Exports running | start a long export, Restart now | "Exports or Imports Are Running", Keep Working installs nothing; Restart Anyway updates |
| Cancel | Cancel during the download | back to Install update; nothing replaced |
| Bad signature | re-sign the package with another generated key, or flip one byte of the package and keep its `.sig`, restart the feed | "does not carry Rawtonic's signature", N keeps running, Try again fails the same way |
| Wrong version | sign with `--app-version` of another version | the same signature message |
| No manifest | restart the feed with `--no-manifest` | no Install update; the Download / Release page link as before |
| Offline | stop the feed during the download | the network message; N unchanged; Try again works after restarting the feed |
| Not updatable | Windows: run N from the portable zip or an all-users copy; macOS: N from the mounted disk image; Linux: N as a .deb | the browser link, with the all-users or move-to-Applications note where it applies |
| No permission | macOS: N in `/Applications` owned by another user, as a standard user; cancel the password prompt. Linux: the AppImage in a folder you cannot write | "may not replace itself here", N keeps running unchanged |
| Quit while installing | Linux: Restart now and close the window at once | afterwards the AppImage is either N or N+1, never partial |
| Windows | Restart now | the setup runs passively without an administrator prompt and starts N+1 |

With the QA control channel (docs/testing/qa-control.md; build with `--features qa-control` and `VITE_RAWTONIC_QA=1`, launch with `--env RAWTONIC_QA_PORT=…`) the clicks and screenshots can be scripted; to see the progress on a local feed, serve the package slowly (a copy of `update_feed.py` that writes the body in 1 MB pieces with a pause: 34 MB downloads in under a second otherwise). The restart keeps the launch environment.

Record OS, build commit, each case's result and the feed log in the task's report. Delete the QA key when done.
