#!/bin/sh
# Every quality gate, run locally before a merge into main (the same checks
# as CI, which runs on Linux after the merge). Stops at the first failure.
# Usage: scripts/check.sh [--quick]   (--quick skips the Rust test suite)
set -eu
cd "$(dirname "$0")/.."
QUICK=0; [ "${1:-}" = "--quick" ] && QUICK=1
step() { printf '\n== %s\n' "$*"; }

step "legal: notices shipped with the app"
cmp THIRD_PARTY_NOTICES.md src-tauri/resources/licenses/THIRD_PARTY_NOTICES.md
cmp licenses/Adobe-DNG-Specification-Patent-License.txt src-tauri/resources/licenses/Adobe-DNG-Specification-Patent-License.txt
python3 scripts/third_party_licenses.py --check
step "frontend: typecheck"; npm run -s typecheck
step "frontend: lint (errors only)"; npm run -s lint -- --quiet
step "frontend: tests"; npm test --silent
step "frontend: i18n"; npm run -s i18n:check
step "frontend: production build"; npm run -s build >/dev/null
step "rust: format"; cargo fmt --all --check
step "rust: clippy"; cargo clippy --workspace --all-targets --locked -- -D warnings
step "rust: layering"; scripts/check_layering.sh
if [ "$QUICK" = 0 ]; then step "rust: tests"; cargo test --workspace --locked; fi
if command -v cargo-deny >/dev/null 2>&1; then
  step "dependencies: licenses, bans, sources"; cargo deny check licenses bans sources
else
  printf '\n== dependencies: skipped (cargo-deny not installed)\n'
fi
printf '\nAll checks passed.\n'
