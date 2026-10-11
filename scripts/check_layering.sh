#!/bin/sh
# Engine layering gate (ADR 0009): crates below the desktop adapter must not
# reach Tauri, its webview stack or rawtonic-app through normal or build
# dependencies, on any target.
set -eu
status=0
for package in rawtonic-runtime rawtonic-rawdecode rawtonic-engine rawtonic-gpu rawtonic-export rawtonic-merge rawtonic-faces rawtonic-catalog rawtonic-metadata \
    rawtonic-naming rawtonic-ingest rawtonic-fileops rawtonic-interop rawtonic-cli; do
    found=$(cargo tree --locked -p "$package" -e normal,build --target all --prefix none \
        --format '{p}' | awk '{print $1}' | grep -E '^(tauri|tauri-.*|wry|tao|webkit2gtk|rawtonic-app)$' |
        sort -u || true)
    if [ -n "$found" ]; then
        echo "$package depends on: $(echo "$found" | tr '\n' ' ')" >&2
        status=1
    fi
done
[ "$status" -eq 0 ] && echo "Layering: no engine or core crate depends on Tauri."
exit "$status"
