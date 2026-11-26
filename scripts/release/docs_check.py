#!/usr/bin/env python3
"""Stage `docs` of scripts/release.sh: do the user docs follow the app?

    docs_check.py --app REPO --prev-tag vPREV --head SHA --version V
                  --website CLONE --site-ref REF [--site-ref REF …]
                  [--map docs_map.json] [--reviewed REASON]
    docs_check.py branches --website CLONE [--name B …]
        The docs branches a release carries, one `NAME COMMIT` per line (below).

Every release checks whether the tools it changes still match their pages on
rawtonic.cc/docs (user decision 2026-11-26). A tool counts as changed since the
previous release when

  * a line inside one of its `<ToolGroup …>` blocks in src/components/studio/
    changed, was added or was removed (removed ones against the old file; the group's `key`, else its title in camelCase: "Lens correction" →
    lensCorrection), or
  * a file matching one of its `paths` in docs_map.json changed, or
  * a bullet of the release's CHANGELOG section names one of its `terms`
    (docs_map.json; whole words, case as written).

A docs page (website `pages/docs/*.html` and `pages/de/docs/*.html`, front matter
`"docs": {"tool": KEY}`) covers the tool KEY and, for a key `group.control`,
changes of the group too. A page counts as updated when its content on one of the
--site-ref refs the release ships (its website branch, the docs branches it
carries, origin/main) differs from the website the previous release shipped (its
`Merge release-PREV:` commit, else its `release: PREV` commit); an edit that was
reverted does not count.

The docs branches a release carries (`branches`): the --name ones (`none`: none),
else every local or origin `docs-*` branch of the website not in origin/main,
each at the newer of its local and origin commits (diverged ones fail).

Fails (exit 1) when a changed tool's page was not updated in both languages,
unless --reviewed gives the reason the pages still hold (it is printed in the
report). Tools without a page are listed, not failures.
"""
import argparse
import fnmatch
import json
import re
import subprocess
import sys
from pathlib import Path

DOC_DIRS = ("pages/docs/", "pages/de/docs/")
STUDIO = "src/components/studio/"


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True).stdout


def camel(title):
    words = re.findall(r"[A-Za-z0-9]+", title)
    return words[0].lower() + "".join(w[:1].upper() + w[1:] for w in words[1:]) if words else ""


def tool_groups(text):
    """(first line, last line, key) of every <ToolGroup> block, 1-based, nested ones too."""
    lines = text.split("\n")
    out, stack = [], []
    for i, line in enumerate(lines, 1):
        for m in re.finditer(r"<(/?)ToolGroup\b", line):
            if m.group(1):
                if stack:
                    start, key = stack.pop()
                    out.append((start, i, key))
                continue
            # The opening tag's attributes: this line and the next ones up to its end.
            head = line[m.end():]
            for j in range(i, min(i + 12, len(lines))):
                if re.search(r"(^|[^=])>\s*$", head) or "/>" in head:
                    break
                head += "\n" + lines[j]
            k = re.search(r'\bkey="([^"]+)"', head)
            t = re.search(r'\btitle="([^"]+)"', head)
            key = k.group(1) if k else camel(t.group(1)) if t else None
            if re.search(r"/>\s*$", head.split("\n")[0]):
                out.append((i, i, key))
            else:
                stack.append((i, key))
    return [g for g in out if g[2]]


def changed_lines(diff):
    """{path: ([old-side lines], [new-side lines])} of a `git diff -U0`: removed lines
    by their number in the old file, added ones by theirs in the new file."""
    out, old, new = {}, None, None
    for line in diff.split("\n"):
        if line.startswith("--- "):
            old = line[6:] if line.startswith("--- a/") else None
        elif line.startswith("+++ "):
            new = line[6:] if line.startswith("+++ b/") else None
        elif line.startswith("@@"):
            m = re.match(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", line)
            a, b, c, d = (int(m.group(i)) if m.group(i) is not None else 1 for i in (1, 2, 3, 4))
            path = new or old
            lines = out.setdefault(path, ([], []))
            if old:
                lines[0].extend(range(a, a + b))
            if new:
                lines[1].extend(range(c, c + d))
    return out


def groups_changed(app, base, head):
    """{tool key: [files]} of the ToolGroup blocks whose lines changed between base and
    head: removed lines against the groups of the old file (a removed group counts),
    added ones against the new file's."""
    diff = git(app, "diff", "-U0", "--no-renames", f"{base}..{head}", "--", STUDIO)
    out = {}
    for path, sides in changed_lines(diff).items():
        if not path.endswith(".tsx") or ".test." in path:
            continue
        for ref, nums in zip((base, head), sides):
            if not nums:
                continue
            groups = tool_groups(git(app, "show", f"{ref}:{path}"))
            for n in nums:
                inner = [g for g in groups if g[0] <= n <= g[1]]
                if inner:
                    out.setdefault(max(inner, key=lambda g: g[0])[2], set()).add(path)
    return {k: sorted(v) for k, v in out.items()}


def changelog_section(text, version):
    m = re.search(rf"^## {re.escape(version)} — .*$", text, re.M) or re.search(r"^## Unreleased\s*$", text, re.M)
    if not m:
        return ""
    end = re.search(r"^## ", text[m.end():], re.M)
    return text[m.end(): m.end() + end.start() if end else len(text)]


def bullets(section):
    return [b.strip() for b in re.split(r"\n(?=- )", "\n" + section) if b.strip().startswith("- ")]


def term_hits(section, terms):
    pats = [re.compile(rf"(?<![\w-]){re.escape(t)}(?![\w-])") for t in terms]
    return [b for b in bullets(section) if any(p.search(b) for p in pats)]


def front_matter(text):
    m = re.match(r"\s*<!--page\s+(\{.*?\})\s*-->", text, re.S)
    return json.loads(m.group(1)) if m else {}


def doc_pages(site, refs):
    """{tool key: {lang: path}} of every docs page on the refs (the last ref with a page wins)."""
    pages = {}
    for ref in refs:
        for path in git(site, "ls-tree", "-r", "--name-only", ref, "--", *DOC_DIRS).split():
            if not path.endswith(".html"):
                continue
            tool = front_matter(git(site, "show", f"{ref}:{path}")).get("docs", {}).get("tool")
            if tool:
                pages.setdefault(tool, {})["de" if path.startswith("pages/de/") else "en"] = path
    return pages


def site_base(site, prev):
    """The website commit that shipped the previous release."""
    v = prev.replace(".", r"\.")
    for grep in (f"^Merge release-{v}:", f"^release: {v}$"):
        c = git(site, "log", "--all", "-1", "--format=%H", "-E", f"--grep={grep}").strip()
        if c:
            return c
    raise SystemExit(f"docs: no website commit of the previous release {prev} ('Merge release-{prev}…' or 'release: {prev}')")


def touched_pages(site, base, refs):
    """Pages whose content on a ref differs from the previous release's (a change that
    was reverted does not count)."""
    out = set()
    for ref in refs:
        out.update(git(site, "diff", "--name-only", "--no-renames", base, ref, "--", *DOC_DIRS).split())
    return out


def docs_branches(site, names):
    """[(name, commit)] of the website docs branches a release carries: the given
    names, else every local or origin docs-* branch not in origin/main. A branch that
    exists locally and on origin is taken where it is newer; diverged ones fail."""
    def commit(ref):
        r = subprocess.run(["git", "-C", str(site), "rev-parse", "-q", "--verify", f"{ref}^{{commit}}"],
                           capture_output=True, text=True)
        return r.stdout.strip() if r.returncode == 0 else ""

    def ancestor(a, b):
        return subprocess.run(["git", "-C", str(site), "merge-base", "--is-ancestor", a, b]).returncode == 0

    if names:
        names = [n for n in names if n != "none"]
    else:
        refs = git(site, "for-each-ref", "--format=%(refname)", "refs/heads/docs-*", "refs/remotes/origin/docs-*").split()
        names = sorted({r.split("/", 3)[3] if r.startswith("refs/remotes/") else r[len("refs/heads/"):] for r in refs})
    out = []
    for n in names:
        local, remote = commit(f"refs/heads/{n}"), commit(f"refs/remotes/origin/{n}")
        if local and remote and local != remote:
            if ancestor(remote, local):
                c = local
            elif ancestor(local, remote):
                c = remote
            else:
                raise SystemExit(f"docs: the website branch {n} differs locally and on origin: push or pull it first")
        else:
            c = local or remote
        if not c:
            raise SystemExit(f"docs: no website branch {n} (local or on origin)")
        if not ancestor(c, "refs/remotes/origin/main"):
            out.append((n, c))
    return out


def covers(page_key, tool):
    return page_key == tool or page_key.startswith(tool + ".")


def check(changed, pages, touched):
    """Rows (tool, why, page state) and the failures."""
    rows, failures = [], []
    for tool in sorted(changed):
        keys = [k for k in pages if covers(k, tool) or covers(tool, k)]
        if not keys:
            rows.append((tool, changed[tool], "no docs page"))
            continue
        for k in keys:
            stale = [lang for lang in ("en", "de") if pages[k].get(lang) not in touched]
            state = "updated" if not stale else "NOT updated: " + ", ".join(
                f"{lang} {pages[k].get(lang, '(missing)')}" for lang in stale)
            rows.append((tool, changed[tool], f"{k}: {state}"))
            if stale:
                failures.append(k)
    return rows, sorted(set(failures))


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ["branches"]:
        bp = argparse.ArgumentParser(prog="docs_check.py branches")
        bp.add_argument("--website", required=True)
        bp.add_argument("--name", action="append", default=[])
        b = bp.parse_args(argv[1:])
        for name, c in docs_branches(b.website, b.name):
            print(name, c)
        return 0
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--app", required=True)
    ap.add_argument("--prev-tag", required=True)
    ap.add_argument("--head", required=True)
    ap.add_argument("--version", required=True)
    ap.add_argument("--website", required=True)
    ap.add_argument("--site-ref", action="append", required=True)
    ap.add_argument("--map", default=str(Path(__file__).with_name("docs_map.json")))
    ap.add_argument("--reviewed", default="")
    a = ap.parse_args(argv)
    tools = json.loads(Path(a.map).read_text())["tools"]

    changed = {}
    for key, files in groups_changed(a.app, a.prev_tag, a.head).items():
        changed.setdefault(key, []).append("Develop/Library group: " + ", ".join(Path(f).name for f in files))
    files = git(a.app, "diff", "--name-only", f"{a.prev_tag}..{a.head}").split()
    section = changelog_section(git(a.app, "show", f"{a.head}:CHANGELOG.md"), a.version)
    for key, spec in tools.items():
        hit = sorted({f for f in files for g in spec.get("paths", [])
                      if fnmatch.fnmatch(f, g) and "test" not in Path(f).name})
        if hit:
            changed.setdefault(key, []).append("files: " + ", ".join(hit[:3]) + (f" (+{len(hit) - 3})" if len(hit) > 3 else ""))
        for b in term_hits(section, spec.get("terms", [])):
            changed.setdefault(key, []).append("CHANGELOG: " + b[2:90].replace("\n", " ") + ("…" if len(b) > 90 else ""))

    prev = a.prev_tag[1:] if a.prev_tag.startswith("v") else a.prev_tag
    base = site_base(a.website, prev)
    pages = doc_pages(a.website, a.site_ref)
    touched = touched_pages(a.website, base, a.site_ref)
    rows, failures = check(changed, pages, touched)

    print(f"docs: tools changed since {a.prev_tag} ({a.head[:9]}); website pages since {base[:9]} on {', '.join(a.site_ref)}")
    for tool, why, state in rows:
        print(f"  {tool}: {state}")
        for w in why:
            print(f"      {w}")
    if not rows:
        print("  (no tool changes found)")
    if failures and a.reviewed:
        print(f"docs: {len(failures)} page(s) not updated, reviewed: {a.reviewed}")
        return 0
    if failures:
        print(f"docs: FAIL — {', '.join(failures)}: update the page(s) on a website docs branch (en and de), "
              "or rerun with --docs-reviewed \"why the pages still hold\"")
        return 1
    print("docs: every changed tool with a page has its page updated")
    return 0


if __name__ == "__main__":
    sys.exit(main())
