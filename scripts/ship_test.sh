#!/bin/sh
# Tests scripts/ship.sh against a throwaway repository: a local bare "origin",
# a main checkout and branch worktrees, with `gh` replaced by a stub. Nothing
# reaches GitHub. Run by scripts/check.sh; usage: scripts/ship_test.sh
set -eu
SHIP=$(cd "$(dirname "$0")" && pwd -P)/ship.sh
T=$(mktemp -d "${TMPDIR:-/tmp}/rawtonic-ship-test.XXXXXX")
T=$(cd "$T" && pwd -P)
WT=$(cd "$(dirname "$0")/.." && pwd -P)
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
# The npm stub below stands in for npm ci (the real one would redo the whole
# install, which defeats check.sh's relay rule). The first run from the
# worktree tests ship.sh itself; later runs are the main branch's restart of
# check.sh and the branches' own check.sh, so they stop after the dry plan.
first_run() { # the run this file was started from
  [ "$(cd "$(dirname "$0")" && pwd -P)" = "$WT/scripts" ]
}
ship_with() { # dir: ship.sh with the plain npm stub and, unless the first run, the dry stop
  # Each pattern must match once, or the copy would ship without the stub.
  for pattern in '^RAWTONIC_SHIP_JS_REPO=.*' '^RAWTONIC_SHIP_NPM=.*' \
    '^  scripts/check.sh --brief$'; do
    [ "$(grep -c "$pattern" "$SHIP")" = 1 ] ||
      fail "ship_test.sh: ship.sh has no single line matching $pattern"
  done
  # shellcheck disable=SC2016  # ship.sh's lines, not this script's
  sed \
    -e 's|^RAWTONIC_SHIP_NPM=.*|RAWTONIC_SHIP_NPM='"'"'npm ci --silent'"'"'|' \
    -e 's|^  scripts/check.sh --brief$|  sh "$0" --dry-run|' \
    "$SHIP" >"$1"
}
cat >"$T/bin/gh" <<'EOF'
#!/bin/sh
# gh stub: no pull request exists until `gh pr create`. `pr view` prints what
# ship.sh's --jq selects: the URL of an open pull request ($GH_STATE.state
# holds another state, as for a merged one), else nothing.
# Any other caller is not ours: exit 0 and do nothing, so a stubbed `| head`
# never turns into a SIGPIPE the script cannot see.
case "$1 $2" in
  "pr view")
    [ -f "$GH_STATE" ] || exit 1
    [ "$(cat "$GH_STATE.state" 2>/dev/null || echo OPEN)" != OPEN ] || cat "$GH_STATE" ;;
  "pr comment") while [ $# -gt 0 ]; do [ "$1" = --body-file ] && cp "$2" "$GH_STATE.comment"; shift; done ;;
  "pr create")
    while [ $# -gt 0 ]; do [ "$1" = --body-file ] && cp "$2" "$GH_STATE.body"; shift; done
    echo https://github.com/rawtonic-app/rawtonic/pull/7 | tee "$GH_STATE" ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$T/bin/gh"
# The temp directory, for the stubs below: ship.sh runs in a copy of itself in
# the caller's environment and does not pass T on.
printf 'T=%s\n' "$T" >"$T/env"
# npm stub: records the call in $T/npm.log (with the directory and NODE_ENV it
# ran in). It installs the lockfile it finds in its directory, so a ship.sh
# that keeps the main checkout's node_modules current leaves a matching hash
# behind. A test that wants one to fail points $T/npm-fail-main at a file the
# stub copies into its directory before exiting 1.
cat >"$T/bin/npm" <<'EOF'
#!/bin/sh
[ -n "${T:-}" ] || . "$(dirname "$0")/../env"
here=$(pwd -P)
printf '%s: %s [NODE_ENV=%s]\n' "$here" "$*" "${NODE_ENV:-unset}" >>"$T/npm.log"
case "$*" in *" ci"*|*" ci "*) ;; *) exit 0 ;; esac
if [ -f "$T/npm-fail-main" ] && [ "$here" = "$(cd "$T/main" && pwd -P)" ]; then
  cp "$T/npm-fail-main" .
  exit 1
fi
mkdir -p node_modules
printf '{"hash": "%s"}\n' "$(sha256sum package-lock.json | cut -d' ' -f1)" >node_modules/.package-lock.json
EOF
chmod +x "$T/bin/npm"
printf '#!/bin/sh\nexit 0\n' >"$T/bin/shellcheck"
chmod +x "$T/bin/shellcheck"
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

# The version of this test that a lock branch of #20/#21 ships as its own
# check.sh: the stale install check first, then the test with main's install
# stub. Main's copy of this test (which main's history lacks) must not read as
# a change of the merge: make_copy recreates it, stand_in recreates the
# check.sh stand-in. The pass stamp is the caller's ($CHECK_STAMP).
make_copy() { # recreate main's copy of this test, with the caller's pass stamp
  sed -e 's|^RAWTONIC_SHIP_NPM=.*|RAWTONIC_SHIP_NPM='"'"'npm ci --silent'"'"'|' \
    -e 's|^  scripts/check.sh --brief$|  sh "$0" --dry-run|' \
    -e 's|^STAMP=.*|STAMP='"'"'$CHECK_STAMP'"'"'|' \
    -e 's|^  scripts/review.sh|  true|' \
    "${SHIP_ROOT:?}/ship.sh" >"$T/main/scripts/ship_test.sh"
}
stand_in() { # recreate main's check.sh stand-in: this part and the test
  tail -n +"$line" "$0" >"$T/main/scripts/check.sh"
  cat "$T/main/scripts/ship_test.sh" >>"$T/main/scripts/check.sh"
}
# Main's scripts/ (main's own tests only): the pass stamp and review verdict
# of the tree the caller stands in.
stale_check() { # exit 1 when an install in $1 is older than its lock
  for d in "$1/app" "$1/services/relay"; do
    [ -f "$d/node_modules/.package-lock.json" ] || continue
    [ -f "$d/package-lock.json" ] || continue
    cmp -s "$d/package-lock.json" "$d/node_modules/.package-lock.json" ||
      { echo "npm error invalid: stale node_modules ($d)"; exit 1; }
  done
}
git init -q --bare "$T/origin.git"
git clone -q "$T/origin.git" "$T/main" 2>/dev/null
# main's own scripts/ship.sh (branches carry it at least up to the fix) and a
# copy of this test as the stand-in for check.sh in the branches' ship.sh: the
# real check.sh, with only these files, skips nearly every gate, runs the test
# again inside check.sh, and needs the js dependencies npm ci installs.
# The pass stamp of a check.sh run below is the branch's, not main's: its
# stand-in (below) writes the one in $CHECK_STAMP.
# shellcheck disable=SC2016  # ship.sh's lines, not this script's
mkdir "$T/main/scripts"
eval "$(sed -n '/^first_run() {/,/^}/p;/^ship_with() {/,/^}/p' "$WT/scripts/ship_test.sh")"
SHIP=$WT/scripts/ship.sh T=$T ship_with "$T/main/scripts/ship.sh"
cp "$WT/scripts/ship_test.sh" "$T/main/scripts/ship_test.sh"
cp "$(dirname "$SHIP")/review.sh" "$T/main/scripts/review.sh"
cp -R "$(dirname "$SHIP")/review" "$T/main/scripts/review"
git -C "$T/main" add scripts
git -C "$T/main" commit -q -m "chore: start"
git -C "$T/main" push -q origin main
# The lock branch's app/ and services/relay/ locks are main's too, with the
# main checkout's own installs (node_modules) ignored: git then leaves them
# out of commits and ship.sh's clean-tree check. A branch is cut from main's
# commit, not from the checkout's working tree.
mkdir -p "$T/main/app" "$T/main/services/relay" "$T/main/app/node_modules" \
  "$T/main/services/relay/node_modules"
printf '{"packages": {"app": "1.0.0"}}\n' >"$T/main/app/package-lock.json"
printf '{"packages": {"relay": "1.0.0"}}\n' >"$T/main/services/relay/package-lock.json"
printf 'node_modules/\n' >"$T/main/.gitignore"
git -C "$T/main" add app services .gitignore
git -C "$T/main" commit -q -m "chore: locks before the branch"
git -C "$T/main" push -q origin main
# Reviewer stubs for scripts/review.sh (RAWTONIC_REVIEWER).
for v in approve changes_requested; do
  # shellcheck disable=SC2016  # the stub's own $1
  printf '#!/bin/sh\ncat >/dev/null\nprintf %s >"$1"\n' \
    "'{\"verdict\": \"$v\", \"summary\": \"stub $v\", \"findings\": []}'" >"$T/bin/review-$v"
  chmod +x "$T/bin/review-$v"
done

# new_branch NAME: a worktree with two commits (the change, the ship.sh and
# check.sh stand-ins) and a recorded check pass and, unless NAME starts with
# "unreviewed", an approving review.
new_branch() {
  git -C "$T/main" worktree add -q -b "$1" "$T/wt-$1" main
  echo "$1" >"$T/wt-$1/$1.txt"
  git -C "$T/wt-$1" add "$1.txt"
  git -C "$T/wt-$1" commit -q -m "feat: $1"
  # Its own ship.sh and check.sh (this test), in a commit of their own: an
  # amend would touch main's commit if main has scripts/ship.sh in it.
  if [ "${NO_SHIPS:-}" != 1 ]; then
    ship_with "$T/wt-$1/scripts/ship.sh"
    # --no-verify: the pre-commit hook would run this test again (and edit
    # the standing worktree).
    [ -z "$(git -C "$T/wt-$1" status --porcelain)" ] ||
      git -C "$T/wt-$1" commit -q --no-verify -am "chore: test stand-in"
  fi
  gd=$(git -C "$T/wt-$1" rev-parse --git-dir); head=$(git -C "$T/wt-$1" rev-parse HEAD)
  echo "$head" >"$gd/rawtonic-check-passed"
  case "$1" in
    unreviewed*) ;;
    *) mkdir -p "$gd/rawtonic-review"
       echo '{"verdict": "approve", "summary": "ok", "findings": []}' >"$gd/rawtonic-review/$head.json" ;;
  esac
  rm -f "$T/gh-$1" "$T/gh-$1.body" "$T/gh-$1.comment" "$T/gh-$1.state"
}
ship() { # name dir args...: worktree NAME's own scripts/ship.sh, run from DIR
  # A restart in a branch's check.sh runs this test again from the main
  # checkout's copy: main's own ship.sh, run from the main checkout.
  name=$1
  if ! first_run && [ -f "$T/main/scripts/ship_test.sh" ]; then
    dir=${2:?required}; src=$T/main/scripts/ship.sh; shift 2
  else
    dir=$2; src=$T/wt-$name/scripts/ship.sh; shift 2
  fi
  # By absolute path (ship.sh finds its repository from its own path, and a
  # relative scripts/ship.sh would make git open DIR/scripts as the worktree).
  (cd "$dir" && GH_STATE="$T/gh-$name" RAWTONIC_SHIP_NPM= \
    sh -c 'unset RAWTONIC_SHIP_NPM; exec sh "$0" "$@"' "$src" "$@") >"$T/out" 2>&1
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
[ "$(git -C "$T/origin.git" log --oneline main | wc -l)" -eq 2 ] || fail "--no-merge changed main"

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
# shellcheck disable=SC2016  # the hook's own $ref
printf '#!/bin/sh\nwhile read -r _ _ ref; do [ "$ref" != refs/heads/main ] || exit 1; done\n' >"$T/origin.git/hooks/pre-receive"
chmod +x "$T/origin.git/hooks/pre-receive"
before=$(git -C "$T/main" rev-parse HEAD)
ship refused "$T/main" && fail "shipped although origin refused main"
expect_last "$T/out" "ship: FAILED during push main (exit 1)*"
[ "$(git -C "$T/main" rev-parse HEAD)" = "$before" ] || fail "refused push left the merge on local main"
rm "$T/origin.git/hooks/pre-receive"
[ -d "$LOCK" ] && fail "lock left behind"

# 17. A change shipped by Codex (CODEX_THREAD_ID) is reviewed by Claude: the
# stored verdict and the pull request name reviewer and author; Codex may not
# review it, and Claude (CLAUDECODE=1) may not review Claude's own change.
review_as() { # env dir args... (review in DIR with only the given agent env)
  e=$1 d=$2; shift 2
  # shellcheck disable=SC2086  # $e is empty or one VAR=value word
  (cd "$d" && env -u CODEX_THREAD_ID -u CLAUDECODE $e RAWTONIC_REVIEWER=review-approve sh scripts/review.sh "$@") >"$T/out" 2>&1
}
new_branch unreviewed-codex
review_as CODEX_THREAD_ID=t "$T/wt-unreviewed-codex" --reviewer codex && fail "codex reviewed codex's change"
review_as CODEX_THREAD_ID=t "$T/wt-unreviewed-codex" || fail "review of codex's change exited $?"
gd=$(git -C "$T/wt-unreviewed-codex" rev-parse --git-dir); head=$(git -C "$T/wt-unreviewed-codex" rev-parse HEAD)
grep -q '"reviewer": "claude"' "$gd/rawtonic-review/$head.json" || fail "codex's change not routed to claude"
grep -q '"author": "codex"' "$gd/rawtonic-review/$head.json" || fail "author not stored"
ship unreviewed-codex "$T/main" || fail "ship of codex's change exited $?"
grep -q "Verdict on .* by claude (author: codex)" "$T/gh-unreviewed-codex.body" || fail "reviewer/author missing from the pull request"
new_branch unreviewed-claude
review_as CLAUDECODE=1 "$T/wt-unreviewed-claude" --reviewer claude && fail "claude reviewed claude's change"
review_as "CLAUDECODE=1 CODEX_THREAD_ID=t" "$T/wt-unreviewed-claude" --reviewer codex && fail "codex inside claude counted as claude"
review_as CLAUDECODE=1 "$T/wt-unreviewed-claude" --author codex --reviewer codex && fail "--author ignored"

# 18. The machine's switch (Codex limit): Claude then reviews Claude's change
# too, marked as a same-model review; switching back restores the refusal.
(cd "$T/wt-unreviewed-claude" && sh scripts/review.sh --default claude) >/dev/null || fail "--default claude exited $?"
(cd "$T/main" && sh scripts/review.sh --default) | grep -q "is claude" || fail "switch not shared across worktrees"
review_as CLAUDECODE=1 "$T/wt-unreviewed-claude" || fail "switched review exited $?"
gd=$(git -C "$T/wt-unreviewed-claude" rev-parse --git-dir); head=$(git -C "$T/wt-unreviewed-claude" rev-parse HEAD)
grep -q '"reviewer": "claude"' "$gd/rawtonic-review/$head.json" || fail "switch did not route to claude"
grep -q "Same-model review" "$gd/rawtonic-review/$head.json" || fail "same-model review not marked"
(cd "$T/main" && sh scripts/review.sh --default codex) >/dev/null || fail "--default codex exited $?"
review_as CLAUDECODE=1 "$T/wt-unreviewed-claude" --reviewer claude && fail "claude reviewed claude's change after switching back"

# 19. The real Claude invocation is read-only: a stub `claude` records its
# arguments and prompt; only read-only tools, no MCP servers, no settings, and
# the diff is in the prompt.
printf '#!/bin/sh\nprintf "%%s\\n" "$@" >"%s/claude-args"\ncat >"%s/claude-prompt"\necho %s\n' "$T" "$T" \
  "'{\"structured_output\": {\"verdict\": \"approve\", \"summary\": \"claude stub\", \"findings\": []}}'" >"$T/bin/claude"
chmod +x "$T/bin/claude"
new_branch unreviewed-real
(cd "$T/wt-unreviewed-real" && env -u RAWTONIC_REVIEWER -u CLAUDECODE CODEX_THREAD_ID=t sh scripts/review.sh) >"$T/out" 2>&1 || fail "stubbed claude review exited $?"
grep -qx "Read,Grep,Glob" "$T/claude-args" || fail "claude reviewer not limited to read-only tools"
grep -qx -- "--strict-mcp-config" "$T/claude-args" || fail "claude reviewer may load MCP servers"
grep -qx -- "--setting-sources" "$T/claude-args" || fail "claude reviewer loads user/project settings"
grep -q "unreviewed-real" "$T/claude-prompt" || fail "diff missing from the claude prompt"
previous=$(git -C "$T/wt-unreviewed-real" rev-parse HEAD)
grep -q '"branch": "unreviewed-real"' "$(git -C "$T/wt-unreviewed-real" rev-parse --git-dir)/rawtonic-review/$previous.json" || fail "branch not stored"
echo 'later-round sentinel' >"$T/wt-unreviewed-real/later.txt"
git -C "$T/wt-unreviewed-real" add later.txt
git -C "$T/wt-unreviewed-real" commit -q -m "fix: later round"
(cd "$T/wt-unreviewed-real" && env -u RAWTONIC_REVIEWER -u CLAUDECODE CODEX_THREAD_ID=t sh scripts/review.sh) >"$T/out" 2>&1 || fail "later claude review exited $?"
sed -n '/## Incremental diff since earlier review/,$p' "$T/claude-prompt" >"$T/incremental"
grep -q '+later-round sentinel' "$T/incremental" || fail "incremental diff missing from Claude prompt"
grep -q 'diff --git a/unreviewed-real.txt' "$T/incremental" && fail "incremental diff includes unchanged branch work"
rm "$T/bin/claude"

# 20./21. Stale node_modules in the main checkout (PR #576): a branch that
# changes a package-lock.json gets bumped out of date by the merge. check.sh
# then fails on a stale install (#20), and ship.sh installs the merged locks
# before the check and main's again when the check fails and the merge is
# undone (#21). Such a branch ships its own copy of this test as check.sh; it
# sees the locks and installs as they are when it starts, and recreates the
# main checkout's own copy before running the test again, so that copy (which
# main's history lacks) does not read as a change of the merge.
commit() { # dir message
  git -C "$1" add -A
  git -C "$1" commit -q -m "$2"
}
lock_branch() { # name: a branch app/ and services/relay/ locks behind main (PR #576)
  NO_SHIPS=1 new_branch "$1"
  mkdir -p "$T/wt-$1/app" "$T/wt-$1/services/relay" "$T/wt-$1/.config"
  printf '{"packages": {"app": "2.0.0"}}\n' >"$T/wt-$1/app/package-lock.json"
  printf '{"packages": {"relay": "2.0.0"}}\n' >"$T/wt-$1/services/relay/package-lock.json"
  printf '[nextest]\n' >"$T/wt-$1/.config/nextest.toml"
  gd=$(git -C "$T/wt-$1" rev-parse --git-dir); head=$(git -C "$T/wt-$1" rev-parse HEAD)
  # The branch's check.sh, from here above: make_copy and stand_in (main's
  # stand-ins for the merge's locks and node_modules), the stale install
  # check of the tree it runs in, then the test, whose exit status it
  # returns (its last statement is that call).
  {
    # Shipped as main's scripts/check.sh: main is where it runs, and where the
    # caller (ship.sh's merge check) stands; $gd is the branch's git dir.
    printf '#!/bin/sh\n'
    printf "CHECK_STAMP=\${CHECK_STAMP:-%s}\n" "$gd/rawtonic-check-passed"
    printf "T=%s WT=%s\n" "$T" "$WT"
    printf "SHIP_ROOT=%s\n" "$WT/scripts"
    printf '%s\n' 'd=${RAWTONIC_SHIP_CALLER:-$PWD}' 'cd "$d"'
    printf '%s\n' 'CHECK_STAMP=$(git rev-parse --git-dir)/rawtonic-check-passed'
    # The functions, and the line of the tail the commit copies (the last
    # statement of this check.sh is that call).
    { sed -n '/^make_copy() {/,/^}/p;/^stand_in() {/,/^}/p;/^stale_check() {/,/^}/p' \
        "$WT/scripts/ship_test.sh"
      grep -v -- '-e ' "$WT/scripts/ship_test.sh" |
        sed -n '/^make_copy() {/,/^}/p;/^stand_in() {/,/^}/p;/^stale_check() {/,/^}/p'
    }
    printf '%s\n' 'line=$((LINENO + 2))' 'stale_check "$PWD"' 'make_copy' 'stand_in' \
      'sh "$T/main/scripts/ship_test.sh"'
  } >"$T/wt-$1/scripts/check.sh"
  chmod +x "$T/wt-$1/scripts/check.sh"
  commit "$T/wt-$1" "feat: $1"
  # main's stand-in for the branch's check.sh, pushed (the branch ships no
  # scripts; main does), and main's own ship.sh, which the branch's check.sh
  # restarts from.
  cp "$T/wt-$1/scripts/check.sh" "$T/main/scripts/check.sh"
  commit "$T/main" "chore: check.sh stand-in"
  git -C "$T/main" push -q origin main
  # Back to main, which lacks check.sh but has its own script: the stand-in
  # removed before the reset, so that it cannot turn into a deletion.
  rm -f "$T/main/scripts/check.sh"
  git -C "$T/main" reset -q --hard HEAD~1
  # main's copy of this test again (the reset also drops it).
  cp "$WT/scripts/ship_test.sh" "$T/main/scripts/ship_test.sh"
  head=$(git -C "$T/wt-$1" rev-parse HEAD)
  echo "$head" >"$gd/rawtonic-check-passed"
  mkdir -p "$gd/rawtonic-review"
  echo '{"verdict": "approve", "summary": "ok", "findings": []}' >"$gd/rawtonic-review/$head.json"
  rm -f "$T/gh-$1" "$T/gh-$1.body" "$T/gh-$1.comment" "$T/gh-$1.state"
}

before=$(git -C "$T/main" rev-parse HEAD)
lock_branch stale

main_install() { # node_modules as the main checkout's install of its locks
  # Only the lock each install reads: check.sh's relay rule is about mtimes.
  for d in app services/relay; do
    mkdir -p "$T/main/$d/node_modules"
    cp "$T/main/$d/package-lock.json" "$T/main/$d/node_modules/.package-lock.json"
  done
}
main_install

# #20: both merged installs run before the check on the merge (#576). That
# stub npm ci would make the merged locks current here, so the shipped copy
# of ship.sh records the installs instead (RAWTONIC_SHIP_NPM), and the check
# on the merge fails as it did in the bug: against the old install.
# (The branch's own check.sh call gets this test's dry stop. Its install skips
# NODE_ENV, as ship.sh's own does; the check, from main's checkout next to
# the relay, not.)
sed \
  -e 's|^  scripts/check.sh --brief[$]|  sh "$0" --dry-run|' \
  -e 's|^  scripts/review.sh|  true|' \
  -e '/^if \[ -n "\$TITLE" \] || TITLE=/a\
exit 0' \
  "$WT/scripts/ship.sh" >"$T/wt-stale/scripts/ship.sh"
# The installs are recorded: the later RAWTONIC_SHIP_NPM line wins.
printf '%s\n' "s|^RAWTONIC_SHIP_NPM=.*|RAWTONIC_SHIP_NPM='echo npm ci in \"\$PWD\" >>\"\$T/npm.log\"'|" \
  >"$T/install.sed"
sed -i -f "$T/install.sed" "$T/wt-stale/scripts/ship.sh"
commit "$T/wt-stale" "chore: the branch's ship.sh"
gd=$(git -C "$T/wt-stale" rev-parse --git-dir); head=$(git -C "$T/wt-stale" rev-parse HEAD)
echo "$head" >"$gd/rawtonic-check-passed"
mkdir -p "$gd/rawtonic-review"
echo '{"verdict": "approve", "summary": "ok", "findings": []}' >"$gd/rawtonic-review/$head.json"
main=$(cd "$T/main" && pwd -P)
before=$(git -C "$T/main" rev-parse HEAD)
if ship stale "$T/main"; then fail "stale node_modules shipped"; fi
expect_last "$T/out" "ship: FAILED during check of the merge with main (exit 1)*"
grep -q 'npm error invalid' "$T/out" || fail "#20: the merged-tree check did not fail on the old install"
[ "$(git -C "$T/main" rev-parse HEAD)" = "$before" ] || fail "#20: failed check left the merge on local main"
ship_with "$T/wt-stale/scripts/ship.sh"
commit "$T/wt-stale" "fix: ship.sh"
gd=$(git -C "$T/wt-stale" rev-parse --git-dir); head=$(git -C "$T/wt-stale" rev-parse HEAD)
echo "$head" >"$gd/rawtonic-check-passed"
echo '{"verdict": "approve", "summary": "ok", "findings": []}' >"$gd/rawtonic-review/$head.json"

# #21: the fix — both locks reinstalled first, the check then sees current
# packages; a failing check restores main's locks and their packages.
new_branch stale2
git -C "$T/wt-stale2" cherry-pick "$(git -C "$T/wt-stale" rev-parse HEAD)" >/dev/null ||
  fail "#21: cherry-pick of the fix failed"
# Main moved after the branch (as for any branch made before a main merge).
printf 'after\n' >"$T/main/after.txt"
git -C "$T/main" add after.txt
git -C "$T/main" commit -q -m "chore: main moves on"
git -C "$T/main" push -q origin main
sed -e 's|^RAWTONIC_SHIP_NPM=.*|RAWTONIC_SHIP_NPM='"'"'npm ci --silent'"'"'|' \
  -e 's|^  scripts/review.sh|  true|' \
  "$SHIP" >"$T/wt-stale2/scripts/ship.sh"
commit "$T/wt-stale2" "chore: test stub"
gd=$(git -C "$T/wt-stale2" rev-parse --git-dir); head=$(git -C "$T/wt-stale2" rev-parse HEAD)
echo "$head" >"$gd/rawtonic-check-passed"
echo '{"verdict": "approve", "summary": "ok", "findings": []}' >"$gd/rawtonic-review/$head.json"
rm -f "$T/npm.log"; cp "$T/main/app/package-lock.json" "$T/npm-fail-main"
ship stale2 "$T/main" && fail "#21: shipped although the check of the merge failed"
expect_last "$T/out" "ship: FAILED during check of the merge with main (exit 1)*"
main=$(cd "$T/main" && pwd -P)
[ "$(sed -n 1p "$T/npm.log")" = "$main/app: ci --silent [NODE_ENV=unset]" ] || fail "#21: app install is not the first npm call: $(cat "$T/npm.log")"
[ "$(sed -n 2p "$T/npm.log")" = "$main/services/relay: ci --silent [NODE_ENV=unset]" ] || fail "#21: relay install not after the app install: $(cat "$T/npm.log")"
[ "$(sed -n 3p "$T/npm.log")" = "$main/app: ci --silent [NODE_ENV=unset]" ] || fail "#21: app packages not restored after the reset: $(cat "$T/npm.log")"
[ "$(sed -n 4p "$T/npm.log")" = "$main/services/relay: ci --silent [NODE_ENV=unset]" ] || fail "#21: relay packages not restored after the reset: $(cat "$T/npm.log")"
[ "$(wc -l <"$T/npm.log")" = 4 ] || fail "#21: unexpected npm calls: $(cat "$T/npm.log")"
git -C "$T/main" diff --quiet "$before" HEAD -- app/package-lock.json services/relay/package-lock.json ||
  fail "#21: main's locks not restored"
rm "$T/npm-fail-main"

if [ "$FAILS" != 0 ]; then
  printf 'last output:\n'; cat "$T/out"
  exit 1
fi
echo "ship.sh: all tests passed"

