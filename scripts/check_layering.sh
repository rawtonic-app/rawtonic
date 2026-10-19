#!/bin/sh
# Engine layering gate (ADR 0009): crates below the desktop adapter must not
# reach Tauri, its webview stack or rawtonic-app through normal or build
# dependencies, on any target. Every workspace package except rawtonic-app is
# checked (from `cargo metadata`, so a new crate needs no entry here), and a
# failing `cargo tree` fails the gate instead of reading as "no dependency".
set -eu
packages=$(cargo metadata --no-deps --format-version 1 --locked | python3 -c '
import json, sys
for p in json.load(sys.stdin)["packages"]:
    if p["name"] != "rawtonic-app":
        print(p["name"])
')
[ -n "$packages" ] || { echo "layering: no workspace packages found" >&2; exit 1; }
status=0
for package in $packages; do
    tree=$(cargo tree --locked -p "$package" -e normal,build --target all --prefix none --format '{p}') ||
        { echo "layering: cargo tree failed for $package" >&2; status=1; continue; }
    found=$(printf '%s\n' "$tree" | awk '{print $1}' |
        grep -E '^(tauri|tauri-.*|wry|tao|webkit2gtk|rawtonic-app)$' | sort -u || true)
    if [ -n "$found" ]; then
        echo "$package depends on: $(echo "$found" | tr '\n' ' ')" >&2
        status=1
    fi
done
[ "$status" -eq 0 ] && echo "Layering: no engine or core crate depends on Tauri ($(echo "$packages" | wc -l | tr -d ' ') crates)."
exit "$status"
