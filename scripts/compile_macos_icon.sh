#!/bin/sh
# Compile the Icon Composer document src-tauri/icons/Rawtonic.icon into
# src-tauri/icons/Assets.car, the layered (Liquid Glass) app icon of macOS 26+.
# The bundler copies the committed Assets.car into the app and sets
# CFBundleIconName, so builds need no Xcode; older macOS uses icon.icns.
# Run after changing Rawtonic.icon. Needs Xcode 26+ (actool); the active
# developer directory may stay on the Command Line Tools.
set -eu
cd "$(dirname "$0")/.."

if [ -z "${DEVELOPER_DIR:-}" ] && ! xcrun --find actool >/dev/null 2>&1; then
  DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
  export DEVELOPER_DIR
fi
major=$(xcrun actool --version --output-format human-readable-text |
  sed -n 's/^short-bundle-version: \([0-9]*\).*/\1/p')
if [ "${major:-0}" -lt 26 ]; then
  echo "compile_macos_icon: actool from Xcode 26 or newer is required" >&2
  exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# Same arguments as the Tauri bundler uses for .icon files.
xcrun actool src-tauri/icons/Rawtonic.icon --compile "$tmp" \
  --output-format human-readable-text --notices --warnings \
  --output-partial-info-plist "$tmp/partial.plist" \
  --app-icon Rawtonic --include-all-app-icons \
  --enable-on-demand-resources NO --development-region en \
  --target-device mac --minimum-deployment-target 26.0 --platform macosx
cp "$tmp/Assets.car" src-tauri/icons/Assets.car
echo "wrote src-tauri/icons/Assets.car"
