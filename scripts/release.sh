#!/bin/bash
# shellcheck disable=SC2329  # the stages are called by name (fn_of)
# One release in one command: the version bump PR, then from the merged
# commit the signed and notarized macOS DMGs (Apple Silicon, Intel), the Linux
# AppImage (Linux build host over SSH) and the Windows installer (GitHub
# workflow) in parallel, every release smoke, the release notes, SHA256SUMS
# and a DRAFT prerelease. It never publishes a release or pushes a tag.
#
#   scripts/release.sh [options] VERSION
#     --dry-run            print the stages and their commands; changes nothing
#     --no-bump            no bump PR: build --sha (default origin/main), which
#                          must already carry VERSION and its CHANGELOG section
#     --sha SHA            the commit to build with --no-bump
#     --no-draft           stop before `gh release create` (notes and sums are
#                          still written)
#     --prev VERSION       the previous release for the identity checks
#                          (default: the newest release on GitHub before VERSION)
#     --date YYYY-MM-DD    the CHANGELOG date (default: today)
#     --from STAGE         redo STAGE and every stage after it
#     --only STAGE         run STAGE alone (its inputs must exist), e.g. gui
#     --accept-ident-diff  an intended export change: identity differences are
#                          listed, not failures (the CHANGELOG must say so)
#     --list               list the stages
#
# Every stage logs to WORK/logs/STAGE.log and leaves WORK/state/STAGE.ok when it
# passed, so a rerun skips what is done. Machine-specific values (signing
# credentials file, Linux host, photographs) come from the environment or the
# untracked file $RAWTONIC_RELEASE_ENV (default ~/.config/rawtonic/release.env):
# docs/RELEASING.md lists them.
set -uo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
SCRIPTS="$REPO_ROOT/scripts/release"

STAGES=(preflight bump source prev
  mac-arm64 dmg-arm64 ident mac-x86_64 dmg-x86_64 qa-app gui relay
  linux linux-smoke
  windows
  creds sums notes draft)

usage() { sed -n '3,/^set -uo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit "${1:-2}"; }

DRY=0 BUMP=1 DRAFT=1 SHA_ARG="" PREV="" DATE="" FROM="" ONLY="" ACCEPT_DIFF=0 VERSION=""
while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) DRY=1; shift ;;
    --no-bump) BUMP=0; shift ;;
    --sha) SHA_ARG=$2; shift 2 ;;
    --no-draft) DRAFT=0; shift ;;
    --prev) PREV=${2#v}; shift 2 ;;
    --date) DATE=$2; shift 2 ;;
    --from) FROM=$2; shift 2 ;;
    --only) ONLY=$2; shift 2 ;;
    --accept-ident-diff) ACCEPT_DIFF=1; shift ;;
    --list) printf '%s\n' "${STAGES[@]}"; exit 0 ;;
    -h|--help) usage 0 ;;
    -*) echo "release: unknown option $1" >&2; usage ;;
    *) [ -z "$VERSION" ] || usage; VERSION=${1#v}; shift ;;
  esac
done
[ -n "$VERSION" ] || usage
[[ $VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "release: $VERSION is not a version" >&2; exit 2; }
[ -n "$SHA_ARG" ] && [ "$BUMP" = 1 ] && { echo "release: --sha needs --no-bump" >&2; exit 2; }
for s in "$FROM" "$ONLY"; do
  [ -z "$s" ] || printf '%s\n' "${STAGES[@]}" | grep -qx -- "$s" || { echo "release: no stage $s (--list)" >&2; exit 2; }
done
DATE=${DATE:-$(date +%F)}

# --- configuration ----------------------------------------------------------
ENV_FILE=${RAWTONIC_RELEASE_ENV:-$HOME/.config/rawtonic/release.env}
# shellcheck disable=SC1090
[ -f "$ENV_FILE" ] && . "$ENV_FILE"
GH_REPO=${RAWTONIC_REPO:-rawtonic-app/rawtonic}
WORK=${RAWTONIC_RELEASE_WORK:-$HOME/rawtonic-release-work}/$VERSION
SIGNING_ENV=${RAWTONIC_SIGNING_ENV:-$HOME/.config/rawtonic/signing.env}
PHOTOS=${RAWTONIC_RELEASE_PHOTOS:-}
PHOTO_SUMS=${RAWTONIC_RELEASE_PHOTOS_SUMS:-${PHOTOS:+$PHOTOS/SHA256SUMS}}
RECIPES=${RAWTONIC_IDENT_RECIPES:-}
QA_ID=${RAWTONIC_QA_IDENTIFIER:-cc.rawtonic.qa.release}
QA_PORT=${RAWTONIC_QA_PORT:-7989}
LINUX_DIR=${RAWTONIC_LINUX_DIR:-rawtonic-release}
LINUX_CACHE=${RAWTONIC_LINUX_CACHE:-$LINUX_DIR/cache}
LINUX_LOCK=${RAWTONIC_LINUX_LOCK:-}
LINUX_PHOTOS=${RAWTONIC_LINUX_PHOTOS:-}
read -ra SSH <<< "${RAWTONIC_LINUX_SSH:-}"
read -ra LOCK <<< "${RAWTONIC_BUILD_LOCK:-}"

STATE=$WORK/state LOGS=$WORK/logs ASSETS=$WORK/assets
WT=$WORK/wt WTQA=$WORK/wtqa PREVD=$WORK/prev
TAG=v$VERSION
BUILD_BRANCH=release-build/$VERSION
RAW_RE='\.(arw|raf|nef|nrw|cr2|cr3|rw2|orf|dng|pef|srw|3fr|iiq|x3f)$'

say() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*"; }
fail() { echo "FAIL: $*"; exit 1; }
need() { local s; for s in "$@"; do [ -f "$STATE/$s.ok" ] || fail "needs stage $s first"; done; }
sha() { cat "$STATE/sha"; }
locked() { if [ ${#LOCK[@]} -gt 0 ]; then "${LOCK[@]}" "$@"; else "$@"; fi; }
# One command line on the Linux host, run by bash whatever its login shell.
remote() {
  [ ${#SSH[@]} -gt 0 ] || fail "RAWTONIC_LINUX_SSH is not set"
  [[ $* != *"'"* ]] || fail "remote command with a quote: $*"
  "${SSH[@]}" -o ServerAliveInterval=30 "bash -c '$*'"
}
remote_step() { "${SSH[@]}" -o ServerAliveInterval=30 bash -s -- "$@" < "$SCRIPTS/linux.sh"; }
PHOTO_LIST=()
photos() { find "$PHOTOS" -maxdepth 1 -type f | grep -iE "$RAW_RE" | sort; }
load_photos() { PHOTO_LIST=(); local f; while IFS= read -r f; do PHOTO_LIST+=("$f"); done < <(photos); }
originals_unchanged() {
  [ -f "$PHOTO_SUMS" ] || fail "no checksum file $PHOTO_SUMS for the photographs"
  (cd "$PHOTOS" && shasum -a 256 -c --quiet "$PHOTO_SUMS") || fail "a photograph changed"
  echo "photographs unchanged ($PHOTO_SUMS)"
}

# --- dry run ----------------------------------------------------------------
describe() {
  case $1 in
    preflight) echo "check tools (git gh node npm cargo hdiutil spctl xcrun python3), gh auth, $SIGNING_ENV, photos, recipes, Linux host (ssh), free disk; find the previous release";;
    bump) if [ "$BUMP" = 0 ]; then echo "skipped (--no-bump): SHA = ${SHA_ARG:-origin/main}; check package.json = $VERSION and '## $VERSION — ' in its CHANGELOG"; else
          echo "git worktree add -b release-$VERSION $WORK/bump origin/main; set $VERSION in package.json, package-lock.json (top entries), src-tauri/Cargo.toml, src-tauri/tauri.conf.json, Cargo.lock (cargo update -p rawtonic-app --offline), both THIRD_PARTY_NOTICES.md rows; CHANGELOG '## Unreleased' → '## $VERSION — $DATE'; commit; scripts/ship.sh --title 'chore(release): $VERSION' → merged SHA"; fi;;
    source) echo "git worktree add --detach $WT SHA; git worktree add --detach $WTQA SHA; npm ci in both";;
    prev) echo "gh release download v<prev> (both DMGs, AppImage, LGPL source tarballs) → $PREVD; mount each DMG read-only, ditto Rawtonic.app out";;
    mac-arm64) echo "(credentials sourced from $SIGNING_ENV in a subshell) ${LOCK[*]} env RAWTONIC_BUNDLE_ONNX=1 npm run package:mac (in $WT) → $ASSETS/Rawtonic_${VERSION}_aarch64.dmg";;
    mac-x86_64) echo "(credentials sourced from $SIGNING_ENV in a subshell) ${LOCK[*]} env RAWTONIC_BUNDLE_ONNX=1 npm run package:mac:intel (in $WT) → $ASSETS/Rawtonic_${VERSION}_x86_64.dmg";;
    dmg-arm64|dmg-x86_64) echo "copy DMG, xattr -w com.apple.quarantine (Safari), spctl -a -t open, stapler validate, hdiutil attach -readonly, ditto app out, spctl -a -t exec (Notarized Developer ID), codesign --verify --deep --strict, lipo, relay string, version; headless JPEG exports of the photos (HOME in scratch) vs the previous release's app";;
    ident) echo "previous vs new arm64 app: each photo × each recipe in ${RECIPES:-RECIPES} → 16-bit TIFF, SHA-256 compared (a difference is re-run with fresh HOMEs)";;
    qa-app) echo "in $WTQA, CARGO_TARGET_DIR=$WORK/qatarget: ${LOCK[*]} scripts/tether/bundle_helper.sh; npx tauri build --features qa-control --bundles app --config <tether conf> --config '{\"identifier\":\"$QA_ID\",…}' (VITE_RAWTONIC_QA=1); cargo build --release -p rawtonic-cli";;
    gui) echo "python3 scripts/release/gui_smoke.py --app <QA app> --cli <rawtonic> --raw <CR3> --port $QA_PORT --version $VERSION --work $WORK/gui (fresh HOME: start, quit; What's new; session; Develop frame; export; quit)";;
    relay) echo "cargo run --release of a scratch crate on $WT/crates/contribute: send one report to the build's relay and delete it";;
    linux) echo "git archive SHA | ${SSH[*]:-ssh HOST} tar -x -C $LINUX_DIR/$VERSION/src; ${SSH[*]:-ssh HOST} 'flock ${LINUX_LOCK:-LOCK} chrt --other 0 packaging/linux/build-appimage.sh $LINUX_CACHE' (scripts/release/linux.sh build); copy AppImage + .zsync back";;
    linux-smoke) echo "scripts/release/linux.sh smoke on the host: relay string, zsync info, xvfb-run host exports vs the previous AppImage, ubuntu:22.04 ubuntu:24.04 debian:12 fedora:42 container exports";;
    windows) echo "git push origin SHA:refs/heads/$BUILD_BRANCH; gh workflow run windows-installer.yml --ref $BUILD_BRANCH; wait; gh run download -n windows-installer; sha256 -c; log has 'WARP refused cleanly'/'uninstaller exit 0'; relay string (7z on the Linux host); delete $BUILD_BRANCH";;
    creds) echo "count every signing credential value (except the public identity hash) in $LOGS and the built apps: must be 0; gitleaks dir $LOGS";;
    sums) echo "LGPL tarballs (previous release or pinned URL, SHA-256 against scripts/tether/build_libgphoto2.sh); shasum -a 256 * > $ASSETS/SHA256SUMS.txt";;
    notes) echo "python3 scripts/release/notes.py: CHANGELOG section $VERSION + the previous release's install/feedback text → $WORK/notes.md";;
    draft) if [ "$DRAFT" = 0 ]; then echo "skipped (--no-draft)"; else echo "gh release create $TAG --draft --prerelease --target SHA --title 'Rawtonic $VERSION' --notes-file $WORK/notes.md $ASSETS/* (never published here)"; fi;;
  esac
}

# --- stages -----------------------------------------------------------------
st_preflight() {
  local t
  for t in git gh node npm cargo python3 shasum hdiutil spctl xcrun codesign lipo ditto; do
    command -v "$t" >/dev/null || fail "$t not found"
  done
  gh auth status >/dev/null 2>&1 || fail "gh is not logged in"
  [ "$(uname -s)" = Darwin ] || fail "the signed DMGs are built on macOS"
  [ -f "$SIGNING_ENV" ] || fail "no signing credentials file $SIGNING_ENV"
  grep -q APPLE_SIGNING_IDENTITY "$SIGNING_ENV" || fail "$SIGNING_ENV sets no APPLE_SIGNING_IDENTITY"
  [ -d "$PHOTOS" ] || fail "RAWTONIC_RELEASE_PHOTOS is not a folder"
  load_photos; [ ${#PHOTO_LIST[@]} -gt 0 ] || fail "no RAW photographs in $PHOTOS"
  echo "photographs: ${#PHOTO_LIST[@]} in $PHOTOS"
  originals_unchanged
  ls "$RECIPES"/*.json >/dev/null 2>&1 || fail "RAWTONIC_IDENT_RECIPES has no *.json recipes"
  [ "$QA_ID" != cc.rawtonic.editor ] || fail "the QA identifier must not be the user's cc.rawtonic.editor"
  remote true || fail "the Linux host does not answer over SSH"
  [ -n "$LINUX_PHOTOS" ] || fail "RAWTONIC_LINUX_PHOTOS is not set"
  remote "test -f $LINUX_PHOTOS/SHA256SUMS && command -v docker flock xvfb-run 7z >/dev/null" \
    || fail "the Linux host lacks $LINUX_PHOTOS/SHA256SUMS or docker/flock/xvfb-run/7z"
  local free; free=$(df -g "$(dirname "$WORK")" | awk 'NR==2 {print $4}')
  echo "free disk: ${free} GB"; [ "$free" -ge 40 ] || fail "less than 40 GB free for the builds"
  git -C "$REPO_ROOT" fetch -q origin
  if [ "$DRAFT" = 1 ] && gh release view "$TAG" -R "$GH_REPO" >/dev/null 2>&1; then fail "a release $TAG exists already (draft or published)"; fi
  if [ -z "$PREV" ]; then
    PREV=$(gh release list -R "$GH_REPO" --exclude-drafts --limit 30 --json tagName -q '.[].tagName' \
      | sed 's/^v//' | grep -vxF "$VERSION" | head -1)
  fi
  [ -n "$PREV" ] || fail "no previous release found (--prev)"
  gh release view "v$PREV" -R "$GH_REPO" >/dev/null || fail "no release v$PREV"
  echo "$PREV" > "$STATE/prev"
  echo "version $VERSION, previous release $PREV, work $WORK"
}

st_bump() {
  if [ "$BUMP" = 0 ]; then
    local s; s=$(git -C "$REPO_ROOT" rev-parse --verify "${SHA_ARG:-origin/main}^{commit}") || fail "no commit ${SHA_ARG:-origin/main}"
    check_release_commit "$s"; echo "$s" > "$STATE/sha"; echo "building $s without a bump"; return
  fi
  local b=$WORK/bump branch=release-$VERSION
  if [ ! -d "$b" ]; then
    git -C "$REPO_ROOT" worktree add -b "$branch" "$b" origin/main || fail "worktree"
  fi
  cd "$b" || fail "no $b"
  if [ "$(git log -1 --format=%s)" != "chore(release): $VERSION" ]; then
    python3 "$SCRIPTS/bump.py" "$VERSION" "$DATE" || fail "bump"
    cargo update -p rawtonic-app --offline || fail "Cargo.lock"
    git add -A || fail "git add"
    git commit -q -m "chore(release): $VERSION" || fail "commit"
  fi
  npm ci --no-audit --no-fund >/dev/null || fail "npm ci"
  cat > "$WORK/bump-pr.md" <<EOF
Version $VERSION: version bump and the CHANGELOG heading \`## $VERSION — $DATE\`.

Made by \`scripts/release.sh\`; the release assets are built from the merge commit.
EOF
  scripts/ship.sh --title "chore(release): $VERSION" --body-file "$WORK/bump-pr.md" | tee "$WORK/ship.out"
  local merged; merged=$(tail -1 "$WORK/ship.out" | sed -n 's/^ship: merged PR #[0-9]* as \([0-9a-f]*\).*/\1/p')
  [ -n "$merged" ] || fail "ship.sh did not merge (see above)"
  git -C "$REPO_ROOT" fetch -q origin
  merged=$(git -C "$REPO_ROOT" rev-parse "$merged")
  check_release_commit "$merged"; echo "$merged" > "$STATE/sha"; echo "merged as $merged"
}
check_release_commit() {
  local v
  v=$(git -C "$REPO_ROOT" show "$1:package.json" | python3 -c 'import json,sys;print(json.load(sys.stdin)["version"])')
  [ "$v" = "$VERSION" ] || fail "$1 has version $v, not $VERSION"
  # (grep -q in a pipe would end git early: pipefail would call that a failure)
  grep -q "^## $VERSION — " <<< "$(git -C "$REPO_ROOT" show "$1:CHANGELOG.md")" || fail "$1 has no CHANGELOG section '## $VERSION — '"
}

worktree_at() {  # DIR SHA
  if [ -d "$1" ]; then
    [ "$(git -C "$1" rev-parse HEAD)" = "$2" ] || fail "$1 exists at another commit; remove it or use another work folder"
  else
    git -C "$REPO_ROOT" worktree add --detach "$1" "$2" || fail "worktree $1"
  fi
  (cd "$1" && npm ci --no-audit --no-fund) || fail "npm ci in $1"
}
st_source() { need bump; worktree_at "$WT" "$(sha)"; worktree_at "$WTQA" "$(sha)"; }

st_prev() {
  need preflight
  local p; p=$(cat "$STATE/prev")
  mkdir -p "$PREVD"
  gh release download "v$p" -R "$GH_REPO" -D "$PREVD" --skip-existing \
    -p '*_aarch64.dmg' -p '*_x86_64.dmg' -p '*_amd64.AppImage' -p '*.tar.*' || fail "download v$p"
  local arch dmg mp
  for arch in aarch64 x86_64; do
    dmg=$(ls "$PREVD"/*_"$arch".dmg) || fail "v$p has no $arch DMG"
    rm -rf "${PREVD:?}/$arch"; mkdir -p "$PREVD/$arch"
    mp=$(mktemp -d "$PREVD/mnt.XXXX")
    hdiutil attach -nobrowse -readonly -mountpoint "$mp" "$dmg" >/dev/null || fail "mount $dmg"
    ditto "$mp/Rawtonic.app" "$PREVD/$arch/Rawtonic.app"; hdiutil detach "$mp" >/dev/null; rmdir "$mp"
    echo "v$p $arch: $(defaults read "$PREVD/$arch/Rawtonic.app/Contents/Info.plist" CFBundleShortVersionString)"
  done
  ls -la "$PREVD"
}

mac_build() {  # arm64 | x86_64
  need source
  local arch=$1 script=package:mac dmg=$WT/target/release/bundle/dmg/Rawtonic_${VERSION}_aarch64.dmg
  [ "$arch" = x86_64 ] && script=package:mac:intel dmg=$WT/target/x86_64-apple-darwin/release/bundle/dmg/Rawtonic_${VERSION}_x86_64.dmg
  rm -f "$dmg"
  cd "$WT" || fail "no $WT"
  # The credentials stay in this subshell; the creds stage checks the logs.
  # shellcheck disable=SC1090
  ( set -a; . "$SIGNING_ENV"; set +a; locked env RAWTONIC_BUNDLE_ONNX=1 npm run "$script" ) || fail "$script"
  [ -f "$dmg" ] || fail "no $dmg"
  mkdir -p "$ASSETS"; cp "$dmg" "$ASSETS/"
  ls -la "$ASSETS/$(basename "$dmg")"
}
st_mac_arm64() { mac_build arm64; }
st_mac_x86_64() { mac_build x86_64; }

exports() {  # APP OUTDIR HOME — default JPEG exports of every photo
  local app=$1 out=$2 home=$3 f b s c
  mkdir -p "$out" "$home"; load_photos
  for f in "${PHOTO_LIST[@]}"; do b=$(basename "$f"); s=$(date +%s)
    HOME=$home "$app/Contents/MacOS/rawtonic-app" export "$f" --output "$out/$b.jpg" --format jpeg > "$out/$b.log" 2>&1 < /dev/null; c=$?
    echo "export $b exit $c $(( $(date +%s) - s ))s $(stat -f %z "$out/$b.jpg" 2>/dev/null) $(shasum -a 256 "$out/$b.jpg" 2>/dev/null | cut -c1-16)"
    [ "$c" = 0 ] && [ -s "$out/$b.jpg" ] || echo "EXPORT-FAILED $b"
  done
}
dmg_smoke() {  # aarch64 | x86_64
  local arch=$1 lipo=arm64; [ "$arch" = x86_64 ] && lipo=x86_64
  need "mac-$([ "$arch" = aarch64 ] && echo arm64 || echo x86_64)" prev
  local s=$WORK/smoke-$arch d mp a line bad=0
  rm -rf "${s:?}"; mkdir -p "$s/apps"
  cp "$ASSETS/Rawtonic_${VERSION}_$arch.dmg" "$s/"; d=$s/Rawtonic_${VERSION}_$arch.dmg
  # A browser's quarantine flag: Gatekeeper must accept the notarized DMG and app with it set.
  xattr -w com.apple.quarantine "0083;$(printf %x "$(date +%s)");Safari;$(uuidgen)" "$d"
  line=$(spctl -a -t open --context context:primary-signature -vv "$d" 2>&1 | tr '\n' ' '); echo "spctl dmg: $line"
  [[ $line == *accepted*"source=Notarized Developer ID"* ]] || { echo "BAD dmg not accepted"; bad=1; }
  line=$(xcrun stapler validate "$d" 2>&1 | tail -1); echo "stapler dmg: $line"; [[ $line == *worked* ]] || bad=1
  mp=$(mktemp -d "$s/mnt.XXXX")
  hdiutil attach -nobrowse -readonly -mountpoint "$mp" "$d" >/dev/null || fail "mount"
  ditto "$mp/Rawtonic.app" "$s/apps/Rawtonic.app"; hdiutil detach "$mp" >/dev/null; rmdir "$mp"
  a=$s/apps/Rawtonic.app
  line=$(xattr -p com.apple.quarantine "$a" 2>/dev/null | cut -d';' -f1); echo "app quarantine inherited: $line"; [ -n "$line" ] || bad=1
  line=$(spctl -a -t exec -vv "$a" 2>&1 | tr '\n' ' '); echo "spctl app: $line"
  [[ $line == *accepted*"source=Notarized Developer ID"* ]] || { echo "BAD app not accepted"; bad=1; }
  codesign --verify --deep --strict "$a" && echo "codesign: ok" || bad=1
  line=$(xcrun stapler validate "$a" 2>&1 | tail -1); echo "stapler app: $line"; [[ $line == *worked* ]] || bad=1
  line=$(lipo -archs "$a/Contents/MacOS/rawtonic-app"); echo "arch: $line"; [ "$line" = "$lipo" ] || bad=1
  line=$(grep -c -a relay.rawtonic.cc "$a/Contents/MacOS/rawtonic-app"); echo "relay strings: $line"; [ "$line" -ge 1 ] || bad=1
  line=$(defaults read "$a/Contents/Info.plist" CFBundleShortVersionString); echo "version: $line"; [ "$line" = "$VERSION" ] || bad=1
  exports "$a" "$s/out" "$s/home" | tee "$s/exports.txt"
  exports "$PREVD/$arch/Rawtonic.app" "$s/out-prev" "$s/home-prev" > "$s/exports-prev.txt"
  grep -q EXPORT-FAILED "$s/exports.txt" && bad=1
  compare_dirs "$s/out-prev" "$s/out" "v$(cat "$STATE/prev")" || [ "$ACCEPT_DIFF" = 1 ] || bad=1
  if [ "$arch" = x86_64 ] && [ -d "$WORK/smoke-aarch64/out" ]; then
    echo "Intel vs Apple Silicon (X-Trans may differ slightly; informational):"
    compare_dirs "$WORK/smoke-aarch64/out" "$s/out" "arm64" || true
  fi
  originals_unchanged
  [ "$bad" = 0 ] || fail "dmg smoke $arch"
  echo "dmg smoke $arch: ok"
}
compare_dirs() {  # OLD NEW LABEL
  local f b n=0 same=0
  for f in "$2"/*.jpg; do b=$(basename "$f"); n=$((n + 1))
    if cmp -s "$1/$b" "$f"; then same=$((same + 1)); else echo "  DIFF vs $3: $b"; fi
  done
  echo "  $same/$n exports identical to $3"; [ "$same" = "$n" ]
}
st_dmg_arm64() { dmg_smoke aarch64; }
st_dmg_x86_64() { dmg_smoke x86_64; }

st_ident() {
  need dmg-arm64 prev
  local old=$PREVD/aarch64/Rawtonic.app/Contents/MacOS/rawtonic-app new=$WORK/smoke-aarch64/apps/Rawtonic.app/Contents/MacOS/rawtonic-app
  local i=$WORK/ident f b r k n=0 same=0 diff=0 failed=0 ho hn
  rm -rf "${i:?}"; mkdir -p "$i/out" "$i/home-old" "$i/home-new"
  one() {  # APP HOME PHOTO RECIPE → hash
    HOME=$2 "$1" export "$3" --output "$i/out/x.tif" --format tiff --tiff-bit-depth 16 --adjustments "$4" > "$i/out/x.log" 2>&1 < /dev/null
    shasum -a 256 "$i/out/x.tif" 2>/dev/null | cut -c1-16; rm -f "$i/out/x.tif"
  }
  load_photos
  for f in "${PHOTO_LIST[@]}"; do b=$(basename "$f")
    for r in "$RECIPES"/*.json; do k=$(basename "$r" .json); n=$((n + 1))
      ho=$(one "$old" "$i/home-old" "$f" "$r"); hn=$(one "$new" "$i/home-new" "$f" "$r")
      if [ -n "$ho" ] && [ "$ho" = "$hn" ]; then same=$((same + 1)); continue; fi
      if [ -z "$ho" ] || [ -z "$hn" ]; then failed=$((failed + 1)); echo "FAILED $b $k old=$ho new=$hn"; continue; fi
      # Confirm with fresh homes: a cache left by an earlier export is not the cause.
      rm -rf "$i/h1" "$i/h2"; mkdir -p "$i/h1" "$i/h2"
      ho=$(one "$old" "$i/h1" "$f" "$r"); hn=$(one "$new" "$i/h2" "$f" "$r")
      if [ "$ho" = "$hn" ]; then same=$((same + 1)); echo "same with fresh homes: $b $k"; else diff=$((diff + 1)); echo "DIFF $b $k"; fi
    done
  done
  echo "ident: $n pairs, $same identical, $diff differ, $failed failed (v$(cat "$STATE/prev") vs $VERSION, arm64, 16-bit TIFF)"
  originals_unchanged
  [ "$failed" = 0 ] || fail "exports failed"
  [ "$diff" = 0 ] || [ "$ACCEPT_DIFF" = 1 ] || fail "saved recipes export differently (--accept-ident-diff only for a documented export change)"
}

st_qa_app() {
  need source
  cd "$WTQA" || fail "no $WTQA"
  export CARGO_TARGET_DIR=$WORK/qatarget RAWTONIC_BUNDLE_ONNX=1 VITE_RAWTONIC_QA=1
  local conf; conf=$(printf '{"identifier":"%s","productName":"Rawtonic QA release"}' "$QA_ID")
  locked bash -c "scripts/tether/bundle_helper.sh && npx tauri build --features qa-control --bundles app \
    --config '$CARGO_TARGET_DIR/tether-bundle/tauri.tether.conf.json' --config '$conf' && \
    cargo build --release --locked -p rawtonic-cli" || fail "QA build"
  local app="$CARGO_TARGET_DIR/release/bundle/macos/Rawtonic QA release.app"
  [ "$(strings "$app/Contents/MacOS/rawtonic-app" | grep -c 'assets/qaControl')" -gt 0 ] || fail "the QA bundle has no QA bridge (dist/ replaced during the build?)"
  rm -rf "$WORK/gui-app"; mkdir -p "$WORK/gui-app"
  ditto "$app" "$WORK/gui-app/Rawtonic QA release.app"; cp "$CARGO_TARGET_DIR/release/rawtonic" "$WORK/gui-app/"
  echo "QA app: $(defaults read "$WORK/gui-app/Rawtonic QA release.app/Contents/Info.plist" CFBundleIdentifier)"
}
st_gui() {
  need qa-app
  local raw=${RAWTONIC_GUI_RAW:-$(photos | grep -iE '\.cr3$' | head -1)}
  [ -f "$raw" ] || fail "no CR3 for the GUI smoke (RAWTONIC_GUI_RAW)"
  python3 "$SCRIPTS/gui_smoke.py" --app "$WORK/gui-app/Rawtonic QA release.app" --cli "$WORK/gui-app/rawtonic" \
    --raw "$raw" --port "$QA_PORT" --version "$VERSION" --work "$WORK/gui" || fail "GUI smoke"
  originals_unchanged
}

st_relay() {
  need source
  local d=$WORK/relay-e2e os_ver; os_ver=$(sw_vers -productVersion)
  mkdir -p "$d/src"
  cat > "$d/Cargo.toml" <<EOF
[package]
name = "relay-e2e"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
rawtonic-contribute = { path = "$WT/crates/contribute" }
serde_json = "1"
tokio = { version = "1", features = ["rt", "macros"] }

[workspace]
EOF
  cat > "$d/src/main.rs" <<EOF
// Release smoke: an optimized build of the contribution core sends one report
// to the build's relay and deletes it again by its id.
use rawtonic_contribute::client::{BUILD_RELAY_URL, RelayClient};
use rawtonic_contribute::payload::{Channel, prepare};
use rawtonic_contribute::scrub::Scrubber;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("relay: {BUILD_RELAY_URL:?}");
    let client = RelayClient::from_build("$VERSION").expect("no relay in this build");
    let scrubber = Scrubber::for_this_machine();
    let payload = serde_json::json!({
        "schema": 2,
        "app": {"version": "$VERSION", "os": "macos", "osVersion": "$os_ver", "arch": "$(uname -m)"},
        "text": "Release smoke test for $VERSION (sent by the maintainer's release check, deleted right away)"
    });
    let prepared = prepare(Channel::Report, payload, &scrubber).expect("prepare");
    let receipt = client.send(&prepared, &scrubber).await.expect("send");
    println!("sent: id={:?} ref={:?}", receipt.id.is_some(), receipt.reference);
    client.delete(receipt.id.as_deref().expect("id")).await.expect("delete");
    println!("deleted");
}
EOF
  (cd "$d" && CARGO_TARGET_DIR=$WORK/relay-target locked cargo run --release -q) | tee "$d/out.txt" || fail "relay round trip"
  grep -q '^deleted$' "$d/out.txt" || fail "the report was not deleted"
}

linux_d() { echo "$LINUX_DIR/$VERSION"; }
st_linux() {
  need bump
  local d; d=$(linux_d)
  remote "mkdir -p $d" || fail "remote folder"
  git -C "$REPO_ROOT" archive --format=tar "$(sha)" | remote "cat > $d/src.tar" || fail "upload source"
  remote_step unpack "$d" || fail "unpack"
  remote_step build "$d" "$LINUX_CACHE" "${RAWTONIC_LINUX_CACHE_SEED:--}" "${LINUX_LOCK:--}" "$VERSION" || fail "AppImage build"
  mkdir -p "$ASSETS"
  local f
  for f in "Rawtonic_${VERSION}_amd64.AppImage" "Rawtonic_${VERSION}_amd64.AppImage.zsync"; do
    remote "cat $d/out/$f" > "$ASSETS/$f" || fail "copy $f"
  done
  (cd "$ASSETS" && remote "cd $d/out && sha256sum Rawtonic_${VERSION}_amd64.AppImage*" | shasum -a 256 -c) || fail "copied AppImage differs"
}
st_linux_smoke() {
  need linux prev
  local d; d=$(linux_d)
  local p; p=$(ls "$PREVD"/*_amd64.AppImage) || fail "no previous AppImage"
  remote "mkdir -p $d/prev && cat > $d/prev/prev.AppImage && chmod +x $d/prev/prev.AppImage" < "$p" || fail "upload previous AppImage"
  remote_step smoke "$d" "$LINUX_PHOTOS" "$VERSION" | tee "$WORK/linux-smoke.txt"
  [ "${PIPESTATUS[0]}" = 0 ] || fail "Linux smoke"
  if grep -q 'DIFF-vs-prev' "$WORK/linux-smoke.txt"; then [ "$ACCEPT_DIFF" = 1 ] || fail "host exports differ from the previous AppImage"; fi
}

st_windows() {
  need bump
  local s; s=$(sha); local w=$WORK/win id="" i
  mkdir -p "$w"
  if [ -f "$STATE/windows-run" ]; then
    id=$(cat "$STATE/windows-run")
    # A remembered run that failed is not waited for again: a rerun starts a new one.
    case $(gh run view "$id" -R "$GH_REPO" --json conclusion -q .conclusion) in
      failure|cancelled|timed_out|startup_failure) echo "run $id failed earlier; starting a new one"; id=""; rm -f "$STATE/windows-run";;
    esac
  fi
  if [ -z "$id" ]; then
    git -C "$REPO_ROOT" push -q origin "$s:refs/heads/$BUILD_BRANCH" || fail "push $BUILD_BRANCH"
    local since; since=$(date -u +%FT%TZ)
    gh workflow run windows-installer.yml -R "$GH_REPO" --ref "$BUILD_BRANCH" || fail "workflow run"
    for i in $(seq 30); do
      id=$(gh run list -R "$GH_REPO" --workflow windows-installer.yml --branch "$BUILD_BRANCH" --event workflow_dispatch \
        --json databaseId,headSha,createdAt -q "[.[] | select(.headSha == \"$s\" and .createdAt >= \"$since\")][0].databaseId")
      [ -n "$id" ] && break; sleep 10
    done
    [ -n "$id" ] || fail "the workflow run did not appear"
    echo "$id" > "$STATE/windows-run"
  fi
  echo "run https://github.com/$GH_REPO/actions/runs/$id"
  local st=""
  for i in $(seq 240); do
    st=$(gh run view "$id" -R "$GH_REPO" --json status,conclusion -q '.status + " " + .conclusion')
    [[ $st == completed* ]] && break; sleep 60
  done
  echo "run $id: $st"
  gh run view "$id" -R "$GH_REPO" --log > "$w/run.log" 2>&1
  [ "$st" = "completed success" ] || fail "the Windows run did not succeed"
  grep -q 'WARP refused cleanly (exit 1)' "$w/run.log" && echo "export: WARP refused cleanly (exit 1)" \
    || grep -q 'export exit 0' "$w/run.log" || fail "the log shows neither the WARP refusal nor an export"
  grep -q 'uninstaller exit 0' "$w/run.log" || fail "no 'uninstaller exit 0'"
  rm -rf "$w/artifact"; gh run download "$id" -R "$GH_REPO" -n windows-installer -D "$w/artifact" || fail "download"
  (cd "$w/artifact" && shasum -a 256 -c SHA256SUMS-windows.txt) || fail "checksum"
  local exe="Rawtonic_${VERSION}_x64-setup.exe"
  [ -f "$w/artifact/$exe" ] || fail "no $exe"
  # The relay address in the installed binary (7z unpacks the NSIS installer; on the Linux host).
  local d; d=$(linux_d)
  remote "rm -rf $d/win && mkdir -p $d/win && cat > $d/win/setup.exe && cd $d/win && 7z x -y -ox setup.exe >/dev/null && grep -c -a relay.rawtonic.cc x/rawtonic-app.exe" \
    < "$w/artifact/$exe" > "$w/relay.txt" || fail "relay string check"
  echo "relay strings in rawtonic-app.exe: $(cat "$w/relay.txt")"; [ "$(cat "$w/relay.txt")" -ge 1 ] || fail "no relay address"
  mkdir -p "$ASSETS"; cp "$w/artifact/$exe" "$w/artifact/SHA256SUMS-windows.txt" "$ASSETS/"
  git -C "$REPO_ROOT" push -q origin --delete "$BUILD_BRANCH" && echo "deleted $BUILD_BRANCH" || echo "note: $BUILD_BRANCH not deleted"
}

st_creds() {
  # Values only, never printed: every credential except the identity, whose hash is public (codesign output).
  local names name val n total=0 f
  names=$(sed -n 's/^[[:space:]]*\(export[[:space:]]*\)\{0,1\}\(APPLE_[A-Z_]*\)=.*/\2/p' "$SIGNING_ENV" | grep -vx APPLE_SIGNING_IDENTITY)
  [ -n "$names" ] || fail "no credential names found in $SIGNING_ENV"
  for f in "$LOGS"/* "$WORK"/smoke-*/apps/Rawtonic.app/Contents/MacOS/rawtonic-app "$WORK/notes.md"; do
    [ -f "$f" ] || continue
    n=0
    for name in $names; do
      # shellcheck disable=SC1090
      val=$(set -a; . "$SIGNING_ENV"; printenv "$name")
      [ ${#val} -ge 6 ] && n=$((n + $(grep -c -a -F -- "$val" "$f")))
    done
    [ "$n" = 0 ] || echo "credential values in $(basename "$f"): $n"
    total=$((total + n))
  done
  echo "credential values found: $total (in $LOGS, the apps, the notes)"
  [ "$total" = 0 ] || fail "credentials leaked"
  if command -v gitleaks >/dev/null; then gitleaks dir "$LOGS" --no-banner --redact || fail "gitleaks"; fi
}

st_sums() {
  need dmg-arm64 dmg-x86_64 linux windows prev
  local pins; pins=$(git -C "$REPO_ROOT" show "$(sha):scripts/tether/build_libgphoto2.sh")
  local lib url want file
  for lib in LIBUSB LIBTOOL GPHOTO; do
    url=$(sed -n "s/^${lib}_URL=//p" <<< "$pins"); want=$(sed -n "s/^${lib}_SHA=//p" <<< "$pins")
    file=$(basename "$url")
    if [ ! -f "$ASSETS/$file" ]; then
      if [ -f "$PREVD/$file" ]; then cp "$PREVD/$file" "$ASSETS/"; else curl -fsSL -o "$ASSETS/$file" "$url" || fail "download $url"; fi
    fi
    [ "$(shasum -a 256 "$ASSETS/$file" | cut -d' ' -f1)" = "$want" ] || fail "$file does not match its pin"
    echo "LGPL source $file: pin ok"
  done
  (cd "$ASSETS" && rm -f SHA256SUMS.txt && shasum -a 256 -- * > "$WORK/SHA256SUMS.txt" && mv "$WORK/SHA256SUMS.txt" .) || fail "sums"
  cat "$ASSETS/SHA256SUMS.txt"
}

st_notes() {
  need sums
  local p; p=$(cat "$STATE/prev")
  gh release view "v$p" -R "$GH_REPO" --json body -q .body > "$WORK/prev-body.md" || fail "previous notes"
  git -C "$REPO_ROOT" show "$(sha):CHANGELOG.md" > "$WORK/CHANGELOG.md"
  python3 "$SCRIPTS/notes.py" --version "$VERSION" --prev "$p" --changelog "$WORK/CHANGELOG.md" \
    --prev-body "$WORK/prev-body.md" --sums "$ASSETS/SHA256SUMS.txt" > "$WORK/notes.md" || fail "notes"
  grep -n -F -e "$HOME" "$WORK/notes.md" && fail "the notes name a private path"
  [ ${#SSH[@]} -gt 0 ] && grep -n -F -e "${SSH[${#SSH[@]}-1]}" "$WORK/notes.md" && fail "the notes name the Linux host"
  wc -c "$WORK/notes.md"
}

st_draft() {
  [ "$DRAFT" = 1 ] || { echo "skipped (--no-draft)"; return; }
  need sums notes creds
  local s; for s in dmg-arm64 dmg-x86_64 ident gui relay linux-smoke windows; do need "$s"; done
  gh release view "$TAG" -R "$GH_REPO" >/dev/null 2>&1 && fail "$TAG exists already"
  gh release create "$TAG" -R "$GH_REPO" --draft --prerelease --target "$(sha)" \
    --title "Rawtonic $VERSION" --notes-file "$WORK/notes.md" "$ASSETS"/* || fail "gh release create"
  gh release view "$TAG" -R "$GH_REPO" --json url,isDraft,assets -q '.url, .isDraft, (.assets[] | .name + " " + (.size|tostring))'
}

# --- runner -----------------------------------------------------------------
fn_of() { echo "st_${1//-/_}"; }
run_stage() {
  local name=$1 t0 rc
  if [ -f "$STATE/$name.ok" ]; then say "$name: done earlier, skipped"; touch "$STATE/$name.skipped"; return 0; fi
  say "$name: start (log $LOGS/$name.log)"
  t0=$(date +%s)
  ( set -e; "$(fn_of "$name")" ) > "$LOGS/$name.log" 2>&1; rc=$?
  local dt=$(( $(date +%s) - t0 ))
  if [ "$rc" = 0 ]; then echo "$dt" > "$STATE/$name.ok"; rm -f "$STATE/$name.fail"; say "$name: ok ($((dt / 60)) min $((dt % 60)) s)"
  else echo "$dt" > "$STATE/$name.fail"; say "$name: FAILED (exit $rc) — tail of $LOGS/$name.log:"; tail -5 "$LOGS/$name.log" | sed 's/^/    /'; fi
  return $rc
}
chain_mac() {
  run_stage mac-arm64 || return 1
  ( run_stage dmg-arm64 && run_stage ident ) & local smokes=$!
  run_stage mac-x86_64 && run_stage dmg-x86_64
  run_stage qa-app && run_stage gui
  run_stage relay
  wait $smokes
}
chain_linux() { run_stage linux && run_stage linux-smoke; }
summary() {
  local s r t total=$(( $(date +%s) - START ))
  printf '\n%-12s %-22s %8s  %s\n' STAGE RESULT TIME LOG
  for s in "${STAGES[@]}"; do
    if [ -f "$STATE/$s.ok" ]; then t=$(cat "$STATE/$s.ok"); r=ok; [ -f "$STATE/$s.skipped" ] && r="ok (earlier run)"
    elif [ -f "$STATE/$s.fail" ]; then t=$(cat "$STATE/$s.fail"); r=FAILED
    else t=""; r="not run"; fi
    printf '%-12s %-22s %8s  %s\n' "$s" "$r" "${t:+$((t / 60))m$(printf %02d $((t % 60)))s}" "$([ -f "$LOGS/$s.log" ] && echo "$LOGS/$s.log")"
  done
  printf '%-12s %-22s %8s\n' total "this run" "$((total / 60))m$(printf %02d $((total % 60)))s"
  [ "$DRAFT" = 1 ] && [ -f "$STATE/draft.ok" ] && echo "Draft: $(gh release view "$TAG" -R "$GH_REPO" --json url -q .url) — review the notes and assets, then publish it by hand."
}

# The whole run is one function, parsed before it starts: editing this file
# during a run cannot change the running one.
main() {
  if [ "$DRY" = 1 ]; then
    echo "release $VERSION (dry run: nothing is changed) — work folder $WORK"
    echo "parallel after 'prev': [mac-arm64 → (dmg-arm64, ident) · mac-x86_64 → dmg-x86_64 · qa-app → gui · relay] ∥ [linux → linux-smoke] ∥ [windows]"
    for s in "${STAGES[@]}"; do printf '\n[%s]\n  %s\n' "$s" "$(describe "$s")"; done
    exit 0
  fi

  mkdir -p "$STATE" "$LOGS"; chmod 700 "$WORK" "$LOGS"; umask 077
  rm -f "$STATE"/*.skipped
  START=$(date +%s)
  if [ -n "$FROM" ]; then
    drop=0; for s in "${STAGES[@]}"; do [ "$s" = "$FROM" ] && drop=1; [ "$drop" = 1 ] && rm -f "$STATE/$s.ok" "$STATE/$s.fail"; done
    case $FROM in preflight|bump|windows) rm -f "$STATE/windows-run";; esac
  fi
  [ -f "$STATE/prev" ] && PREV=${PREV:-$(cat "$STATE/prev")}
  if [ -n "$ONLY" ]; then
    rm -f "$STATE/$ONLY.ok"; [ "$ONLY" = windows ] && rm -f "$STATE/windows-run"
    run_stage "$ONLY"; rc=$?; summary; exit $rc
  fi
  ok=1
  for s in preflight bump source prev; do run_stage "$s" || { ok=0; break; }; done
  if [ "$ok" = 1 ]; then
    chain_mac & m=$!; chain_linux & l=$!; run_stage windows & w=$!
    wait $m; wait $l; wait $w
    run_stage creds; run_stage sums && run_stage notes && run_stage draft
  fi
  summary
  for s in "${STAGES[@]}"; do [ -f "$STATE/$s.ok" ] || exit 1; done
}
main; exit $?
