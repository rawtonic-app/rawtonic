#!/bin/sh
# Ship the current branch: full check → push → pull request → merge into main
# → delete the branch (remote and local) and its worktree. One command instead
# of a dozen agent steps.
# Usage (from the branch's worktree, clean tree):
#   scripts/ship.sh [--title "type(scope): summary"] [--body-file FILE]
#                   [--no-merge] [--keep-worktree] [--dry-run]
#   --title          PR title (default: the branch's first commit subject)
#   --body-file      PR description (default: the branch's commit subjects)
#   --no-merge       stop after opening the PR (review first)
#   --keep-worktree  keep the worktree and the local branch after the merge
#   --dry-run        check the preconditions and print the plan; pushes,
#                    opens, merges and deletes nothing
# The worktree is never removed while the caller's working directory is inside
# it or the caller runs in an Orca worktree (ORCA_WORKTREE_ID): an agent would
# lose its working directory and could not send its final report. It is then
# kept together with its local branch (checked out there), and the last line
# names the command that removes both later.
# The full `scripts/check.sh --brief` runs unless it already passed on this
# exact HEAD with a clean tree (stamp written by check.sh). Stops at the first
# problem; nothing is merged unless every step before it succeeded.
# The last line of output always says the outcome: `ship: merged PR #N as
# <sha>; …`, `ship: opened PR #N …`, `ship: dry run …` (exit 0) or
# `ship: FAILED …` (exit ≠ 0).
set -eu

# The worktree (and this file) may be removed at the end: run from a copy.
if [ -z "${RAWTONIC_SHIP_COPY:-}" ]; then
  copy=$(mktemp "${TMPDIR:-/tmp}/rawtonic-ship.XXXXXX")
  cp "$0" "$copy"
  RAWTONIC_SHIP_COPY=$copy RAWTONIC_SHIP_CALLER=$(pwd -P) \
    RAWTONIC_SHIP_REPO=$(cd "$(dirname "$0")/.." && pwd -P) exec sh "$copy" "$@"
fi
STEP=start; DONE=0
finish() {
  status=$?
  rm -f "$RAWTONIC_SHIP_COPY"
  if [ "$DONE" = 0 ]; then
    [ "$status" != 0 ] || status=1
    printf 'ship: FAILED during %s (exit %s); nothing after that step ran\n' "$STEP" "$status" >&2
  fi
  exit "$status"
}
trap finish EXIT
cd "$RAWTONIC_SHIP_REPO"

TITLE=''; BODY_FILE=''; MERGE=1; KEEP=0; DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --title) TITLE=$2; shift 2 ;;
    --body-file) BODY_FILE=$2; shift 2 ;;
    --no-merge) MERGE=0; shift ;;
    --keep-worktree) KEEP=1; shift ;;
    --dry-run) DRY=1; shift ;;
    *) echo "ship: unknown option: $1" >&2; exit 2 ;;
  esac
done

say() { printf '== %s\n' "$*"; }
warn() { printf 'ship: warning: %s\n' "$*" >&2; }
die() { printf 'ship: %s\n' "$*" >&2; exit 1; }
# The outcome line; marks the run as finished for the EXIT trap.
result() { DONE=1; printf 'ship: %s\n' "$*"; }

STEP=preconditions
BRANCH=$(git symbolic-ref --short HEAD) || die "detached HEAD"
[ "$BRANCH" != main ] || die "on main; ship a feature branch"
[ -z "$(git status --porcelain)" ] || die "uncommitted changes; commit them first"
git fetch -q origin
[ -n "$(git log --oneline origin/main..HEAD)" ] || die "no commits ahead of origin/main"
WORKTREE=$(pwd -P)
MAIN_DIR=$(git worktree list --porcelain | awk '/^worktree /{w=substr($0,10)} /^branch refs\/heads\/main$/{print w; exit}')
[ -n "$MAIN_DIR" ] || die "no worktree has main checked out"

# Why the worktree stays after the merge (empty: it is removed).
KEEP_WHY=''
case "$RAWTONIC_SHIP_CALLER/" in
  "$WORKTREE"/*) KEEP_WHY="the caller's working directory is inside it" ;;
esac
[ -z "${ORCA_WORKTREE_ID:-}" ] || KEEP_WHY="running in an Orca worktree"
[ "$KEEP" = 0 ] || KEEP_WHY="--keep-worktree"
REMOVE_CMD="git -C '$MAIN_DIR' worktree remove '$WORKTREE' && git -C '$MAIN_DIR' branch -d '$BRANCH'"

STEP=check
HEAD_SHA=$(git rev-parse HEAD)
STAMP="$(git rev-parse --git-dir)/rawtonic-check-passed"
if [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$HEAD_SHA" ]; then
  say "check: already passed on $HEAD_SHA"
elif [ "$DRY" = 1 ]; then
  say "check: would run scripts/check.sh --brief (no pass recorded for $HEAD_SHA)"
else
  say "check"
  scripts/check.sh --brief
fi

[ -n "$TITLE" ] || TITLE=$(git log --reverse --format=%s origin/main..HEAD | head -n 1)
if [ "$DRY" = 1 ]; then
  say "would push $BRANCH and open or reuse a pull request: $TITLE"
  if [ "$MERGE" = 0 ]; then
    result "dry run OK; would stop after the pull request (--no-merge)"
  elif [ -n "$KEEP_WHY" ]; then
    result "dry run OK; would merge $BRANCH into main ($MAIN_DIR) and keep the worktree at $WORKTREE ($KEEP_WHY)"
  else
    result "dry run OK; would merge $BRANCH into main ($MAIN_DIR) and remove the worktree at $WORKTREE"
  fi
  exit 0
fi

STEP=push
say "push $BRANCH"
git push -q -u origin "$BRANCH"

STEP="pull request"
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
if [ "$MERGE" = 0 ]; then
  result "opened PR #$PR_NUM ($PR_URL); not merged (--no-merge)"
  exit 0
fi

STEP=merge
say "merge into main ($MAIN_DIR)"
[ -z "$(git -C "$MAIN_DIR" status --porcelain --untracked-files=no)" ] || die "main checkout has uncommitted changes; merge by hand"
git -C "$MAIN_DIR" pull -q --ff-only origin main
SUMMARY=$(printf '%s' "$TITLE" | sed 's/^[a-z]*\(([^)]*)\)\{0,1\}!\{0,1\}: //')
if ! git -C "$MAIN_DIR" merge -q --no-ff "$BRANCH" -m "Merge pull request #$PR_NUM: $SUMMARY"; then
  git -C "$MAIN_DIR" merge --abort
  die "merge conflict with main; rebase $BRANCH onto origin/main and ship again"
fi
STEP="push main"
git -C "$MAIN_DIR" push -q origin main
MERGED=$(git -C "$MAIN_DIR" rev-parse --short HEAD)

# Merged and pushed: from here on a failed clean-up step only warns.
STEP="clean up"
say "clean up"
cd "$MAIN_DIR"
git push -q origin --delete "$BRANCH" || warn "remote branch not deleted; run: git push origin --delete $BRANCH"
if [ -n "$KEEP_WHY" ]; then
  # The local branch stays: it is checked out in the kept worktree.
  result "merged PR #$PR_NUM as $MERGED ($PR_URL); worktree kept at $WORKTREE ($KEEP_WHY); remove it later with: $REMOVE_CMD"
elif git worktree remove "$WORKTREE" && git branch -d "$BRANCH"; then
  result "merged PR #$PR_NUM as $MERGED ($PR_URL); worktree and branch removed"
else
  warn "worktree or local branch not removed"
  result "merged PR #$PR_NUM as $MERGED ($PR_URL); clean up by hand: $REMOVE_CMD"
fi
