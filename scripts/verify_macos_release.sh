#!/bin/sh
# Verify a Developer ID + notarized release, after packaging. Does not modify it.
# Usage: scripts/verify_macos_release.sh path/to/Rawtonic.app path/to/Rawtonic.dmg
set -eu
[ "$#" = 2 ] || { echo "usage: $0 APP DMG" >&2; exit 2; }
APP=$1
DMG=$2
[ -d "$APP" ] && [ -f "$DMG" ] || { echo "APP or DMG is missing" >&2; exit 1; }
codesign --verify --deep --strict --verbose=2 "$APP"
codesign --verify --strict --verbose=2 "$DMG"
spctl -a -t exec -vv "$APP"
xcrun stapler validate "$APP"
xcrun stapler validate "$DMG"
spctl -a -t open --context context:primary-signature -vv "$DMG"
