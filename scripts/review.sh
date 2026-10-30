#!/bin/sh
# Second-model review: another model (Codex by default) reviews the branch's
# change, or a plan, before it is merged or built. scripts/ship.sh requires an
# approving review of the exact HEAD it merges.
# Usage (from the branch's worktree, clean tree):
#   scripts/review.sh [--base REF]   review the commits REF..HEAD (default origin/main)
#   scripts/review.sh --plan FILE    review a plan before implementing it
#   --reviewer codex|claude          who reviews (default: the machine's switch)
#   --author codex|claude|human      who wrote the change (default: detected from
#                                    the calling agent, see below)
#   scripts/review.sh --default codex|claude   set the switch for every worktree
#                                    of this clone (`--default` alone shows it)
# Exit: 0 approve, 1 changes requested, 2 usage, 3 reviewer unavailable.
# A verdict on the whole branch (default base) is stored as
# $(git rev-parse --git-dir)/rawtonic-review/<HEAD>.json; scripts/ship.sh reads
# it and puts its summary into the pull request. A review with another --base
# covers only part of the branch and is not stored.
# The reviewer runs read-only (`codex exec --sandbox read-only`) with the same
# model and reasoning effort on every machine (RAWTONIC_REVIEW_MODEL,
# RAWTONIC_REVIEW_EFFORT override them), so a verdict does not depend on
# which host's ~/.codex/config.toml happened to run it. The author is the agent
# that runs the review (agents ship their own branches): Codex sets
# CODEX_THREAD_ID in its shells, Claude Code sets CLAUDECODE=1. A change written
# by Codex is reviewed by Claude
# instead (`claude -p` with only Read/Grep/Glob, no MCP servers or settings;
# RAWTONIC_CLAUDE_REVIEW_MODEL), since
# the reviewer must be another model than the author. When Codex's usage
# limit runs short, `--default claude` switches the reviewer to Claude for
# Claude's own commits too; that is a same-model review, and the verdict
# says so in the pull request. `--default codex` switches back. For tests,
# RAWTONIC_REVIEWER names a command that reads the prompt on stdin and writes
# the verdict JSON (scripts/review/verdict.schema.json) to the file in $1.
set -eu
cd "$(dirname "$0")/.."
DIR=scripts/review

PREVIOUS=''
BASE=origin/main; PLAN=''; FULL=1; REVIEWER=''; AUTHOR=''
SWITCH="$(git rev-parse --path-format=absolute --git-common-dir)/rawtonic-reviewer"
while [ $# -gt 0 ]; do
  case "$1" in
    --default)
      case "${2:-}" in
        codex|claude) echo "$2" >"$SWITCH"; echo "review: default reviewer is now $2 (this clone)"; exit 0 ;;
        '') echo "review: default reviewer is $(cat "$SWITCH" 2>/dev/null || echo codex)"; exit 0 ;;
        *) echo "review: --default takes codex or claude" >&2; exit 2 ;;
      esac ;;
    --base) BASE=$2; FULL=0; shift 2 ;;
    --plan) PLAN=$2; shift 2 ;;
    --reviewer) REVIEWER=$2; shift 2 ;;
    --author) AUTHOR=$2; shift 2 ;;
    *) echo "review: unknown option: $1" >&2; exit 2 ;;
  esac
done

PROMPT=$(mktemp "${TMPDIR:-/tmp}/rawtonic-review.XXXXXX")
OUT=$(mktemp "${TMPDIR:-/tmp}/rawtonic-verdict.XXXXXX")
LOG=$(mktemp "${TMPDIR:-/tmp}/rawtonic-review-log.XXXXXX")
DIFF=$(mktemp "${TMPDIR:-/tmp}/rawtonic-review-diff.XXXXXX")
trap 'rm -f "$PROMPT" "$OUT" "$LOG" "$DIFF"' EXIT

DEFAULT=$(cat "$SWITCH" 2>/dev/null || echo codex)
if [ -n "$PLAN" ]; then
  : "${REVIEWER:=$DEFAULT}"
  [ -f "$PLAN" ] || { echo "review: no such plan: $PLAN" >&2; exit 2; }
  { cat "$DIR/plan_prompt.md"; printf '\n## The plan (%s)\n\n' "$PLAN"; cat "$PLAN"; } >"$PROMPT"
else
  [ -z "$(git status --porcelain)" ] || { echo "review: uncommitted changes; commit them first" >&2; exit 2; }
  git rev-parse -q --verify "$BASE" >/dev/null || { echo "review: unknown base $BASE" >&2; exit 2; }
  [ -n "$(git log --oneline "$BASE"..HEAD)" ] || { echo "review: no commits in $BASE..HEAD" >&2; exit 2; }
  HEAD_SHA=$(git rev-parse HEAD)
  BRANCH=$(git branch --show-current)
  if [ "$FULL" = 1 ]; then
    PREVIOUS=$(python3 "$DIR/earlier_round.py" "$(git rev-parse --git-dir)/rawtonic-review" "$HEAD_SHA" --previous)
  fi
  # Codex first: a Codex started from a Claude Code shell inherits CLAUDECODE.
  if [ -z "$AUTHOR" ]; then
    if [ -n "${CODEX_THREAD_ID:-}" ]; then AUTHOR=codex
    elif [ "${CLAUDECODE:-}" = 1 ]; then AUTHOR=claude
    else AUTHOR=human
    fi
  fi
  case "$AUTHOR" in
    codex|claude|human) ;;
    *) echo "review: unknown author: $AUTHOR" >&2; exit 2 ;;
  esac
  if [ "$DEFAULT" = claude ] && [ "$REVIEWER" != codex ]; then
    # Switched to Claude (Codex's limit): Claude reviews everything; for
    # Claude's own change that is a same-model review, said in the verdict.
    REVIEWER=claude
    [ "$AUTHOR" != claude ] || SAME_MODEL=1
  elif [ "$AUTHOR" = codex ]; then
    [ "$REVIEWER" != codex ] || { echo "review: a change written by Codex needs another reviewer (--reviewer claude)" >&2; exit 2; }
    REVIEWER=claude
  elif [ "$REVIEWER" = claude ] && [ "$AUTHOR" = claude ]; then
    echo "review: a change written by Claude needs another reviewer (default codex)" >&2; exit 2
  fi
  {
    cat "$DIR/prompt.md"
    printf '\n## The change\n\nReview the commits `%s..HEAD` (HEAD %s). Read them with `git log %s..HEAD` and `git diff %s...HEAD`.\n\n' \
      "$BASE" "$HEAD_SHA" "$BASE" "$BASE"
    git log --format='- %s' "$BASE"..HEAD
    printf '\n'
    git diff --stat "$BASE"...HEAD
    # A later round checks the earlier findings and the commits since then.
    [ "$FULL" != 1 ] || python3 "$DIR/earlier_round.py" "$(git rev-parse --git-dir)/rawtonic-review" "$HEAD_SHA"
  } >"$PROMPT"
fi

case "${REVIEWER:=codex}" in
  codex|claude) ;;
  *) echo "review: unknown reviewer: $REVIEWER" >&2; exit 2 ;;
esac

if [ -n "${RAWTONIC_REVIEWER:-}" ]; then
  # shellcheck disable=SC2086  # a command with arguments on purpose
  $RAWTONIC_REVIEWER "$OUT" <"$PROMPT" || { echo "review: reviewer command failed" >&2; exit 3; }
elif [ "$REVIEWER" = claude ]; then
  command -v claude >/dev/null 2>&1 || { echo "review: claude not found (Claude Code CLI)" >&2; exit 3; }
  MODEL=${RAWTONIC_CLAUDE_REVIEW_MODEL:-claude-opus-5-5}
  printf '== review: claude %s (read-only) on %s\n' "$MODEL" "${PLAN:-$BASE..HEAD}"
  # Read-only by construction: only the read-only built-in tools (no Bash: even
  # `git diff --output` writes), no MCP servers, no user/project settings and
  # their permission allowances; the commits and the diff go into the prompt.
  if [ -z "$PLAN" ]; then
    python3 "$DIR/verified_diff.py" --base "$BASE" --head "$HEAD_SHA" >"$DIFF" || {
      echo "review: input verification failed; no review was requested" >&2; exit 3;
    }
    { printf '\n## Commits\n\n'; git log "$BASE"..HEAD; printf '\n## Diff\n\n'; cat "$DIFF"; } >>"$PROMPT"
  fi
  if [ -n "$PREVIOUS" ]; then
    python3 "$DIR/verified_diff.py" --base "$PREVIOUS" --head "$HEAD_SHA" --exact-base >"$DIFF" || {
      echo "review: incremental input verification failed; no review was requested" >&2; exit 3;
    }
    { printf '\n## Incremental diff since earlier review (%s to %s)\n\n' "$PREVIOUS" "$HEAD_SHA"; cat "$DIFF"; } >>"$PROMPT"
  fi
  claude -p --model "$MODEL" --tools Read,Grep,Glob --setting-sources '' --strict-mcp-config \
    --permission-mode dontAsk --output-format json --json-schema "$(cat "$DIR/verdict.schema.json")" \
    <"$PROMPT" >"$LOG" 2>&1 &&
    python3 -c 'import json, sys; json.dump(json.load(open(sys.argv[1]))["structured_output"], open(sys.argv[2], "w"))' "$LOG" "$OUT" ||
    { tail -n 20 "$LOG" >&2; echo "review: claude failed (logged in? usage limit?)" >&2; exit 3; }
elif command -v codex >/dev/null 2>&1; then
  MODEL=${RAWTONIC_REVIEW_MODEL:-gpt-6.1-sol}; EFFORT=${RAWTONIC_REVIEW_EFFORT:-high}
  printf '== review: codex %s (%s, read-only) on %s\n' "$MODEL" "$EFFORT" "${PLAN:-$BASE..HEAD}"
  codex exec --sandbox read-only --ephemeral --color never -C "$(pwd -P)" \
    -m "$MODEL" -c "model_reasoning_effort=\"$EFFORT\"" \
    --output-schema "$DIR/verdict.schema.json" -o "$OUT" - <"$PROMPT" >"$LOG" 2>&1 ||
    { tail -n 20 "$LOG" >&2; echo "review: codex failed (logged in? usage limit? switch with scripts/review.sh --default claude)" >&2; exit 3; }
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
    python3 -c 'import json, sys; v = json.load(open(sys.argv[1])); v["reviewer"] = sys.argv[3]; v["author"] = sys.argv[5]; v["branch"] = sys.argv[6]
if sys.argv[4]: v["summary"] = "Same-model review (reviewer switched to Claude, Codex limit). " + v["summary"]
json.dump(v, open(sys.argv[2], "w"), indent=1)' \
      "$OUT" "$STORE/$HEAD_SHA.json" "$REVIEWER" "${SAME_MODEL:-}" "$AUTHOR" "$BRANCH"
  fi
fi
printf 'review: %s\n' "$VERDICT"
[ "$VERDICT" = approve ]
