#!/bin/sh
# Ship the current branch: full check → push → pull request → merge into main
# → delete branch and worktree. One command instead of a dozen agent steps.
# Usage (from the branch's worktree, clean tree):
#   scripts/ship.sh [--title "type(scope): summary"] [--body-file FILE] [--no-merge]
#   --title      PR title (default: the branch's first commit subject)
#   --body-file  PR description (default: the branch's commit subjects)
#   --no-merge   stop after opening the PR (review first)
# The full `scripts/check.sh --brief` runs unless it already passed on this
# exact HEAD with a clean tree (stamp written by check.sh). Stops at the first
# problem; nothing is merged unless every step before it succeeded.
set -eu

# The worktree (and this file) is removed at the end: run from a copy.
if [ -z "${RAWTONIC_SHIP_COPY:-}" ]; then
  copy=$(mktemp "${TMPDIR:-/tmp}/rawtonic-ship.XXXXXX")
  cp "$0" "$copy"
  RAWTONIC_SHIP_COPY=$copy RAWTONIC_SHIP_REPO=$(cd "$(dirname "$0")/.." && pwd) exec sh "$copy" "$@"
fi
trap 'rm -f "$RAWTONIC_SHIP_COPY"' EXIT
cd "$RAWTONIC_SHIP_REPO"

TITLE=''; BODY_FILE=''; MERGE=1
while [ $# -gt 0 ]; do
  case "$1" in
    --title) TITLE=$2; shift 2 ;;
    --body-file) BODY_FILE=$2; shift 2 ;;
    --no-merge) MERGE=0; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

say() { printf '== %s\n' "$*"; }
die() { printf 'ship: %s\n' "$*" >&2; exit 1; }

BRANCH=$(git symbolic-ref --short HEAD) || die "detached HEAD"
[ "$BRANCH" != main ] || die "on main; ship a feature branch"
[ -z "$(git status --porcelain)" ] || die "uncommitted changes; commit them first"
git fetch -q origin
[ -n "$(git log --oneline origin/main..HEAD)" ] || die "no commits ahead of origin/main"
WORKTREE=$(pwd)
MAIN_DIR=$(git worktree list --porcelain | awk '/^worktree /{w=substr($0,10)} /^branch refs\/heads\/main$/{print w; exit}')
[ -n "$MAIN_DIR" ] || die "no worktree has main checked out"

HEAD_SHA=$(git rev-parse HEAD)
STAMP="$(git rev-parse --git-dir)/rawtonic-check-passed"
if [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$HEAD_SHA" ]; then
  say "check: already passed on $HEAD_SHA"
else
  say "check"
  scripts/check.sh --brief
fi

say "push $BRANCH"
git push -q -u origin "$BRANCH"

[ -n "$TITLE" ] || TITLE=$(git log --reverse --format=%s origin/main..HEAD | head -n 1)
if PR_URL=$(gh pr view "$BRANCH" --repo rawtonic-app/rawtonic --json url --jq .url 2>/dev/null); then
  say "pull request exists: $PR_URL"
else
  if [ -z "$BODY_FILE" ]; then
    BODY_FILE=$(mktemp "${TMPDIR:-/tmp}/rawtonic-pr.XXXXXX")
    git log --reverse --format='- %s' origin/main..HEAD >"$BODY_FILE"
  fi
  PR_URL=$(gh pr create --repo rawtonic-app/rawtonic --base main --head "$BRANCH" --title "$TITLE" --body-file "$BODY_FILE")
  say "pull request: $PR_URL"
fi
PR_NUM=${PR_URL##*/}
[ "$MERGE" = 1 ] || { echo "$PR_URL"; exit 0; }

say "merge into main ($MAIN_DIR)"
[ -z "$(git -C "$MAIN_DIR" status --porcelain --untracked-files=no)" ] || die "main checkout has uncommitted changes; merge by hand"
git -C "$MAIN_DIR" pull -q --ff-only origin main
SUMMARY=$(printf '%s' "$TITLE" | sed 's/^[a-z]*\(([^)]*)\)\{0,1\}!\{0,1\}: //')
if ! git -C "$MAIN_DIR" merge -q --no-ff "$BRANCH" -m "Merge pull request #$PR_NUM: $SUMMARY"; then
  git -C "$MAIN_DIR" merge --abort
  die "merge conflict with main; rebase $BRANCH onto origin/main and ship again"
fi
git -C "$MAIN_DIR" push -q origin main
MERGED=$(git -C "$MAIN_DIR" rev-parse --short HEAD)

say "clean up"
cd "$MAIN_DIR"
if [ "$WORKTREE" != "$MAIN_DIR" ]; then git worktree remove "$WORKTREE"; fi
git branch -d "$BRANCH"
git push -q origin --delete "$BRANCH"
printf '\nShipped %s as main %s (%s).\n' "$BRANCH" "$MERGED" "$PR_URL"
