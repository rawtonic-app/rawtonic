#!/bin/sh
# Every quality gate, run locally before a merge into main (the same checks
# as CI, which runs on Linux after the merge). Stops at the first failure.
# Usage: scripts/check.sh [--quick] [--brief] [--changed]
#   --quick    skip the Rust test suite
#   --brief    one line per step; a failing step prints only its last 40 lines
#              (full log path given) — keeps agent context small
#   --changed  only the gates and Rust packages (plus their dependents) the
#              change against origin/main (or $CHECK_BASE) touches
#              (scripts/changed_scope.py);
#              for use while working — run the full check before merging
# Rust tests use cargo-nextest when installed (faster), else cargo test.
set -eu
cd "$(dirname "$0")/.."
QUICK=0; BRIEF=0; CHANGED=0
for arg in "$@"; do
  case "$arg" in
    --quick) QUICK=1 ;;
    --brief) BRIEF=1 ;;
    --changed) CHANGED=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

FRONTEND=1; RUST=1; RUST_PKGS=''; DEPS=1
if [ "$CHANGED" = 1 ]; then
  eval "$(python3 scripts/changed_scope.py)"
  printf 'changed scope: frontend=%s rust=%s deps=%s packages=%s\n' \
    "$FRONTEND" "$RUST" "$DEPS" "${RUST_PKGS:-all}"
fi

LOGDIR=$(mktemp -d "${TMPDIR:-/tmp}/rawtonic-check.XXXXXX")
trap 'rmdir "$LOGDIR" 2>/dev/null || true' EXIT  # keeps a failed --brief log
run() { # name command...
  name=$1; shift
  start=$(date +%s)
  if [ "$BRIEF" = 1 ]; then
    log="$LOGDIR/$(printf '%s' "$name" | tr -c 'A-Za-z0-9' '_').log"
    if "$@" >"$log" 2>&1; then
      printf 'ok   %-45s %4ss\n' "$name" "$(($(date +%s) - start))"
    else
      printf 'FAIL %-45s %4ss\n' "$name" "$(($(date +%s) - start))"
      tail -n 40 "$log"
      printf '(full log: %s)\n' "$log"
      exit 1
    fi
  else
    printf '\n== %s\n' "$name"
    "$@"
    printf '   (%ss)\n' "$(($(date +%s) - start))"
  fi
}

# Helpers chain with && because `set -e` does not apply inside a function
# called from `if` (the --brief path).
legal() {
  cmp THIRD_PARTY_NOTICES.md src-tauri/resources/licenses/THIRD_PARTY_NOTICES.md &&
    cmp licenses/Adobe-DNG-Specification-Patent-License.txt src-tauri/resources/licenses/Adobe-DNG-Specification-Patent-License.txt &&
    python3 scripts/third_party_licenses.py --check
}
frontend_build() { npm run -s build >/dev/null; }
# Rust always builds the whole workspace: selecting packages with -p resolves
# other features and rebuilds dependencies (measured slower than a warm full
# run). --changed only narrows which tests nextest runs.
rust_tests() {
  if [ -z "$RUST_PKGS" ] || ! cargo nextest --version >/dev/null 2>&1; then
    if ! cargo nextest --version >/dev/null 2>&1; then
      cargo test --workspace --locked
      return
    fi
    # fixture_render (harness = false) is excluded in .config/nextest.toml and
    # run by cargo test; nextest does not run doc tests.
    cargo nextest run --workspace --locked --no-fail-fast &&
      cargo test --workspace --locked --doc &&
      cargo test --workspace --locked --test fixture_render
    return
  fi
  # Changed scope: those packages' tests; doc tests are left to the full check.
  filter=$(printf '%s\n' $RUST_PKGS | sed 's/.*/package(&)/' | paste -sd '|' -)
  cargo nextest run --workspace --locked --no-fail-fast --no-tests=pass -E "$filter"
}

run "legal: notices shipped with the app" legal
if [ "$FRONTEND" = 1 ]; then
  run "frontend: typecheck" npm run -s typecheck
  run "frontend: lint (errors only)" npm run -s lint -- --quiet
  run "frontend: tests" npm test --silent
  run "frontend: i18n" npm run -s i18n:check
  run "frontend: production build" frontend_build
fi
if [ "$RUST" = 1 ]; then
  run "rust: format" cargo fmt --all --check
  run "rust: clippy" cargo clippy --workspace --all-targets --locked -- -D warnings
  run "rust: layering" scripts/check_layering.sh
  if [ "$QUICK" = 0 ]; then run "rust: tests" rust_tests; fi
fi
if [ "$DEPS" = 1 ]; then
  if command -v cargo-deny >/dev/null 2>&1; then
    run "dependencies: licenses, bans, sources" cargo deny check licenses bans sources
    # Vulnerabilities and unsound crates fail; unmaintained crates only warn here
    # (they often need an upstream release first) and block releases instead
    # (ADR 0005: the release workflows run the full check).
    run "dependencies: advisories (unmaintained warns)" cargo deny check -W unmaintained advisories
  else
    printf '\n== dependencies: skipped (cargo-deny not installed)\n'
  fi
fi
rm -rf "$LOGDIR"
if [ "$CHANGED" = 1 ]; then
  printf '\nChanged-scope checks passed (run the full check before merging).\n'
else
  printf '\nAll checks passed.\n'
fi
