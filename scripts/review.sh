#!/bin/sh
# Second-model review: another model (Codex by default) reviews the branch's
# change, or a plan, before it is merged or built. scripts/ship.sh requires an
# approving review of the exact HEAD it merges.
# Usage (from the branch's worktree, clean tree):
#   scripts/review.sh [--base REF]   review the commits REF..HEAD (default origin/main)
#   scripts/review.sh --plan FILE    review a plan before implementing it
# Exit: 0 approve, 1 changes requested, 2 usage, 3 reviewer unavailable.
# A verdict on the whole branch (default base) is stored as
# $(git rev-parse --git-dir)/rawtonic-review/<HEAD>.json; scripts/ship.sh reads
# it and puts its summary into the pull request. A review with another --base
# covers only part of the branch and is not stored.
# The reviewer runs read-only (`codex exec --sandbox read-only`) with the same
# model and reasoning effort on every machine (RAWTONIC_REVIEW_MODEL,
# RAWTONIC_REVIEW_EFFORT override them), so a verdict does not depend on
# which host's ~/.codex/config.toml happened to run it. For tests,
# RAWTONIC_REVIEWER names a command that reads the prompt on stdin and writes
# the verdict JSON (scripts/review/verdict.schema.json) to the file in $1.
set -eu
cd "$(dirname "$0")/.."
DIR=scripts/review

BASE=origin/main; PLAN=''; FULL=1
while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE=$2; FULL=0; shift 2 ;;
    --plan) PLAN=$2; shift 2 ;;
    *) echo "review: unknown option: $1" >&2; exit 2 ;;
  esac
done

PROMPT=$(mktemp "${TMPDIR:-/tmp}/rawtonic-review.XXXXXX")
OUT=$(mktemp "${TMPDIR:-/tmp}/rawtonic-verdict.XXXXXX")
LOG=$(mktemp "${TMPDIR:-/tmp}/rawtonic-review-log.XXXXXX")
trap 'rm -f "$PROMPT" "$OUT" "$LOG"' EXIT

if [ -n "$PLAN" ]; then
  [ -f "$PLAN" ] || { echo "review: no such plan: $PLAN" >&2; exit 2; }
  { cat "$DIR/plan_prompt.md"; printf '\n## The plan (%s)\n\n' "$PLAN"; cat "$PLAN"; } >"$PROMPT"
else
  [ -z "$(git status --porcelain)" ] || { echo "review: uncommitted changes; commit them first" >&2; exit 2; }
  git rev-parse -q --verify "$BASE" >/dev/null || { echo "review: unknown base $BASE" >&2; exit 2; }
  [ -n "$(git log --oneline "$BASE"..HEAD)" ] || { echo "review: no commits in $BASE..HEAD" >&2; exit 2; }
  HEAD_SHA=$(git rev-parse HEAD)
  {
    cat "$DIR/prompt.md"
    printf '\n## The change\n\nReview the commits `%s..HEAD` (HEAD %s). Read them with `git log %s..HEAD` and `git diff %s...HEAD`.\n\n' \
      "$BASE" "$HEAD_SHA" "$BASE" "$BASE"
    git log --format='- %s' "$BASE"..HEAD
    printf '\n'
    git diff --stat "$BASE"...HEAD
  } >"$PROMPT"
fi

if [ -n "${RAWTONIC_REVIEWER:-}" ]; then
  # shellcheck disable=SC2086  # a command with arguments on purpose
  $RAWTONIC_REVIEWER "$OUT" <"$PROMPT" || { echo "review: reviewer command failed" >&2; exit 3; }
elif command -v codex >/dev/null 2>&1; then
  MODEL=${RAWTONIC_REVIEW_MODEL:-gpt-6.1-sol}; EFFORT=${RAWTONIC_REVIEW_EFFORT:-high}
  printf '== review: codex %s (%s, read-only) on %s\n' "$MODEL" "$EFFORT" "${PLAN:-$BASE..HEAD}"
  codex exec --sandbox read-only --ephemeral --color never -C "$(pwd -P)" \
    -m "$MODEL" -c "model_reasoning_effort=\"$EFFORT\"" \
    --output-schema "$DIR/verdict.schema.json" -o "$OUT" - <"$PROMPT" >"$LOG" 2>&1 ||
    { tail -n 20 "$LOG" >&2; echo "review: codex failed (logged in? usage limit?)" >&2; exit 3; }
else
  echo "review: codex not found; install it (npm i -g @openai/codex) and run 'codex login'" >&2
  exit 3
fi

VERDICT=$(python3 -c '
import json, sys
v = json.load(open(sys.argv[1]))
print(v["verdict"])
print("summary:", v["summary"], file=sys.stderr)
for f in v["findings"]:
    print("  [%s] %s:%s %s" % (f["severity"], f["file"], f["line"], f["issue"]), file=sys.stderr)
' "$OUT") || { echo "review: unreadable verdict" >&2; exit 3; }

if [ -z "$PLAN" ]; then
  # The verdict belongs to the commit that was reviewed, not to a later one.
  [ "$(git rev-parse HEAD)" = "$HEAD_SHA" ] && [ -z "$(git status --porcelain)" ] ||
    { echo "review: the branch changed during the review; review again" >&2; exit 2; }
  if [ "$FULL" = 1 ]; then
    STORE="$(git rev-parse --git-dir)/rawtonic-review"
    mkdir -p "$STORE"
    cp "$OUT" "$STORE/$HEAD_SHA.json"
  fi
fi
printf 'review: %s\n' "$VERDICT"
[ "$VERDICT" = approve ]
