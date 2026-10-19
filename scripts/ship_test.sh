#!/bin/sh
# Tests scripts/ship.sh against a throwaway repository: a local bare "origin",
# a main checkout and branch worktrees, with `gh` replaced by a stub. Nothing
# reaches GitHub. Run by scripts/check.sh; usage: scripts/ship_test.sh
set -eu
SHIP=$(cd "$(dirname "$0")" && pwd -P)/ship.sh
T=$(mktemp -d "${TMPDIR:-/tmp}/rawtonic-ship-test.XXXXXX")
T=$(cd "$T" && pwd -P)
trap 'rm -rf "$T"' EXIT
# Isolated from the user's git configuration (hooks, signing, hooksPath).
export GIT_CONFIG_GLOBAL="$T/gitconfig" GIT_CONFIG_NOSYSTEM=1 TMPDIR="$T"
# A ship.sh that runs check.sh (and so this test) passes its own state on;
# with it the test's ship.sh would work in the calling repository and run
# check.sh there again, recursing.
unset ORCA_WORKTREE_ID RAWTONIC_SHIP_COPY RAWTONIC_SHIP_CALLER RAWTONIC_SHIP_REPO
git config --global user.name Test
git config --global user.email test@example.org
git config --global init.defaultBranch main

mkdir "$T/bin"
cat >"$T/bin/gh" <<'EOF'
#!/bin/sh
# gh stub: no pull request exists until `gh pr create`. `pr view` prints what
# ship.sh's --jq selects: the URL of an open pull request ($GH_STATE.state
# holds another state, as for a merged one), else nothing.
case "$1 $2" in
  "pr view")
    [ -f "$GH_STATE" ] || exit 1
    [ "$(cat "$GH_STATE.state" 2>/dev/null || echo OPEN)" != OPEN ] || cat "$GH_STATE" ;;
  "pr comment") while [ $# -gt 0 ]; do [ "$1" = --body-file ] && cp "$2" "$GH_STATE.comment"; shift; done ;;
  "pr create")
    while [ $# -gt 0 ]; do [ "$1" = --body-file ] && cp "$2" "$GH_STATE.body"; shift; done
    echo https://github.com/rawtonic-app/rawtonic/pull/7 | tee "$GH_STATE" ;;
  *) echo "gh stub: unexpected: $*" >&2; exit 1 ;;
esac
EOF
chmod +x "$T/bin/gh"
export PATH="$T/bin:$PATH"

FAILS=0
fail() { printf 'FAIL: %s\n' "$*"; FAILS=$((FAILS + 1)); }
expect_last() { # file pattern
  last=$(tail -n 1 "$1")
  # shellcheck disable=SC2254  # $2 is a glob on purpose
  case "$last" in
    $2) ;;
    *) fail "last line '$last' does not match '$2'" ;;
  esac
}

git init -q --bare "$T/origin.git"
git clone -q "$T/origin.git" "$T/main" 2>/dev/null
mkdir "$T/main/scripts"
cp "$SHIP" "$T/main/scripts/ship.sh"
cp "$(dirname "$SHIP")/review.sh" "$T/main/scripts/review.sh"
cp -R "$(dirname "$SHIP")/review" "$T/main/scripts/review"
git -C "$T/main" add scripts
git -C "$T/main" commit -q -m "chore: start"
git -C "$T/main" push -q origin main

# Reviewer stubs for scripts/review.sh (RAWTONIC_REVIEWER).
for v in approve changes_requested; do
  printf '#!/bin/sh\ncat >/dev/null\nprintf %s >"$1"\n' \
    "'{\"verdict\": \"$v\", \"summary\": \"stub $v\", \"findings\": []}'" >"$T/bin/review-$v"
  chmod +x "$T/bin/review-$v"
done

# new_branch NAME: a worktree with one commit and a recorded check pass and,
# unless NAME starts with "unreviewed", an approving review.
new_branch() {
  git -C "$T/main" worktree add -q -b "$1" "$T/wt-$1" main
  echo "$1" >"$T/wt-$1/$1.txt"
  git -C "$T/wt-$1" add "$1.txt"
  git -C "$T/wt-$1" commit -q -m "feat: $1"
  gd=$(git -C "$T/wt-$1" rev-parse --git-dir); head=$(git -C "$T/wt-$1" rev-parse HEAD)
  echo "$head" >"$gd/rawtonic-check-passed"
  case "$1" in
    unreviewed*) ;;
    *) mkdir -p "$gd/rawtonic-review"
       echo '{"verdict": "approve", "summary": "ok", "findings": []}' >"$gd/rawtonic-review/$head.json" ;;
  esac
  rm -f "$T/gh-$1" "$T/gh-$1.body" "$T/gh-$1.comment" "$T/gh-$1.state"
}
ship() { # name dir args... (runs ship.sh of worktree NAME from DIR)
  name=$1 dir=$2; shift 2
  (cd "$dir" && GH_STATE="$T/gh-$name" sh "$T/wt-$name/scripts/ship.sh" "$@") >"$T/out" 2>&1
}

# 1. --dry-run pushes nothing.
new_branch dry
ship dry "$T/wt-dry" --dry-run || fail "dry run exited $?"
expect_last "$T/out" "ship: dry run OK; would merge dry into main * and keep the worktree at $T/wt-dry *"
git -C "$T/origin.git" rev-parse -q --verify refs/heads/dry >/dev/null && fail "dry run pushed the branch"

# 2. A dirty tree fails with a FAILED line and exit 1.
echo x >"$T/wt-dry/dirty.txt"
if ship dry "$T/wt-dry"; then fail "dirty tree shipped"; fi
expect_last "$T/out" "ship: FAILED during preconditions (exit 1)*"
grep -q "uncommitted changes" "$T/out" || fail "dirty tree: no reason given"

# 3. --no-merge opens the PR and leaves main alone.
new_branch review
ship review "$T/wt-review" --no-merge || fail "--no-merge exited $?"
expect_last "$T/out" "ship: opened PR #7 (https://github.com/rawtonic-app/rawtonic/pull/7); not merged (--no-merge)"
git -C "$T/origin.git" rev-parse -q --verify refs/heads/review >/dev/null || fail "--no-merge did not push the branch"
[ "$(git -C "$T/origin.git" log --oneline main | wc -l)" -eq 1 ] || fail "--no-merge changed main"

# 4. Shipping from inside the worktree merges and keeps the worktree and branch.
new_branch inside
ship inside "$T/wt-inside/scripts" --title "feat(x): inside" || fail "ship from inside exited $?"
expect_last "$T/out" "ship: merged PR #7 as * worktree kept at $T/wt-inside (the caller's working directory is inside it); remove it later with: *"
[ -d "$T/wt-inside" ] || fail "worktree the caller was in was removed"
git -C "$T/main" rev-parse -q --verify refs/heads/inside >/dev/null || fail "checked-out local branch was deleted"
git -C "$T/origin.git" rev-parse -q --verify refs/heads/inside >/dev/null && fail "remote branch not deleted"
[ "$(git -C "$T/origin.git" log -1 --format=%s main)" = "Merge pull request #7: inside" ] || fail "origin/main lacks the merge"
cmd=$(tail -n 1 "$T/out" | sed 's/.*remove it later with: //')
sh -c "$cmd" >/dev/null 2>&1 || fail "printed removal command failed: $cmd"
[ -d "$T/wt-inside" ] && fail "printed removal command left the worktree"

# 5. Orca worktree: kept even when called from elsewhere.
new_branch orca
export ORCA_WORKTREE_ID=test
ship orca "$T/main" || fail "ship in Orca worktree exited $?"
unset ORCA_WORKTREE_ID
expect_last "$T/out" "ship: merged PR #7 as *worktree kept at $T/wt-orca (running in an Orca worktree)*"
[ -d "$T/wt-orca" ] || fail "Orca worktree was removed"

# 6. Called from outside the worktree: worktree and branch removed.
new_branch outside
ship outside "$T/main" || fail "ship from outside exited $?"
expect_last "$T/out" "ship: merged PR #7 as *; worktree and branch removed"
[ -d "$T/wt-outside" ] && fail "worktree not removed"
git -C "$T/main" rev-parse -q --verify refs/heads/outside >/dev/null && fail "local branch not deleted"

# 7. --keep-worktree keeps it as well.
new_branch keep
ship keep "$T/main" --keep-worktree || fail "--keep-worktree exited $?"
expect_last "$T/out" "ship: merged PR #7 as *worktree kept at $T/wt-keep (--keep-worktree)*"
[ -d "$T/wt-keep" ] || fail "--keep-worktree removed the worktree"

# 8. No recorded review: the reviewer runs; a request for changes stops the merge.
new_branch unreviewed1
RAWTONIC_REVIEWER=review-changes_requested ship unreviewed1 "$T/main" && fail "shipped without approval"
expect_last "$T/out" "ship: FAILED during review (exit 1)*"
git -C "$T/origin.git" rev-parse -q --verify refs/heads/unreviewed1 >/dev/null && fail "unapproved branch was pushed"

# 9. An approving reviewer: merged, and the verdict is in the pull request.
RAWTONIC_REVIEWER=review-approve ship unreviewed1 "$T/main" || fail "approved ship exited $?"
expect_last "$T/out" "ship: merged PR #7 as *"
grep -q "stub approve" "$T/gh-unreviewed1.body" || fail "review summary missing from the pull request"

# 10. --no-review merges without a reviewer and says why in the pull request.
new_branch unreviewed2
RAWTONIC_REVIEWER=false ship unreviewed2 "$T/main" --no-review "docs only" || fail "--no-review exited $?"
expect_last "$T/out" "ship: merged PR #7 as *"
grep -q "Skipped for .*: docs only" "$T/gh-unreviewed2.body" || fail "--no-review reason missing from the pull request"

# 11. A partial review (--base) is not stored and so does not approve the branch.
new_branch unreviewed3
echo more >"$T/wt-unreviewed3/more.txt"
git -C "$T/wt-unreviewed3" add more.txt
git -C "$T/wt-unreviewed3" commit -q -m "feat: more"
git -C "$T/wt-unreviewed3" rev-parse HEAD >"$(git -C "$T/wt-unreviewed3" rev-parse --git-dir)/rawtonic-check-passed"
(cd "$T/wt-unreviewed3" && RAWTONIC_REVIEWER=review-approve sh scripts/review.sh --base HEAD~1) >/dev/null 2>&1 ||
  fail "partial review exited $?"
RAWTONIC_REVIEWER=review-changes_requested ship unreviewed3 "$T/main" && fail "partial review approved the whole branch"
expect_last "$T/out" "ship: FAILED during review (exit 1)*"

# 12. An existing pull request (opened with --no-merge) gets the new verdict as a comment.
new_branch reship
ship reship "$T/wt-reship" --no-merge || fail "--no-merge exited $?"
ship reship "$T/main" || fail "re-ship exited $?"
grep -q "Verdict on" "$T/gh-reship.comment" || fail "existing pull request got no review comment"

# 13. A merged pull request of the same branch name is not reused: a new one is
# opened and merged under its own number.
new_branch again
echo https://github.com/rawtonic-app/rawtonic/pull/5 >"$T/gh-again"
echo MERGED >"$T/gh-again.state"
ship again "$T/main" || fail "re-used branch name exited $?"
expect_last "$T/out" "ship: merged PR #7 as *"
[ -f "$T/gh-again.comment" ] && fail "review note went to the merged pull request"
[ -f "$T/gh-again.body" ] || fail "no new pull request for a re-used branch name"

# 14. Another ship.sh holds the merge lock: nothing is merged.
new_branch locked
LOCK="$T/main/.git/rawtonic-ship.lock"
mkdir "$LOCK"
ship locked "$T/main" && fail "shipped while another ship held the lock"
expect_last "$T/out" "ship: FAILED during merge (exit 1)*"
[ -d "$LOCK" ] || fail "a failed ship removed the lock it did not take"
rmdir "$LOCK"
[ "$(git -C "$T/origin.git" log -1 --format=%s main)" = "Merge pull request #7: again" ] || fail "merged while locked"

# 15. A local main with commits that are not on origin/main: nothing is pushed.
new_branch ahead
git -C "$T/main" commit -q --allow-empty -m "chore: unpushed"
ship ahead "$T/main" && fail "shipped with an unpushed commit on main"
expect_last "$T/out" "ship: FAILED during merge (exit 1)*"
git -C "$T/origin.git" log --format=%s main | grep -q unpushed && fail "pushed main's unpushed commit"
git -C "$T/main" reset -q --hard origin/main

# 16. origin refuses the push of main: the local merge is undone.
new_branch refused
printf '#!/bin/sh\nwhile read -r _ _ ref; do [ "$ref" != refs/heads/main ] || exit 1; done\n' >"$T/origin.git/hooks/pre-receive"
chmod +x "$T/origin.git/hooks/pre-receive"
before=$(git -C "$T/main" rev-parse HEAD)
ship refused "$T/main" && fail "shipped although origin refused main"
expect_last "$T/out" "ship: FAILED during push main (exit 1)*"
[ "$(git -C "$T/main" rev-parse HEAD)" = "$before" ] || fail "refused push left the merge on local main"
rm "$T/origin.git/hooks/pre-receive"
[ -d "$LOCK" ] && fail "lock left behind"

if [ "$FAILS" != 0 ]; then
  printf 'last output:\n'; cat "$T/out"
  exit 1
fi
echo "ship.sh: all tests passed"
