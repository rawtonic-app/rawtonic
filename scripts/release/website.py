#!/usr/bin/env python3
"""The website's side of a release (run by scripts/release.sh).

    website.py write --version V --changelog CHANGELOG.md --assets DIR --release-json PATH
        Stage `site`: writes the release facts into the website's release.json —
        version, date and dateText (from the CHANGELOG heading `## V — DATE`), tag,
        and every file of the release (mac, macIntel, linux, windows, otherAssets)
        with its size in MiB (rounded half-up) and its SHA-256 from DIR/SHA256SUMS.txt.
        Other keys (whatsNew, _comment) stay as they are. Then fails unless
        whatsNew is this version's, for every language the site has.

    website.py verify-page --version V --lang en|de PAGE.html
        Stage `site-verify`: the fetched home page shows V in its download facts
        and in the What's new heading.

    website.py verify-manifest --version V latest.json
        Stage `site-verify`: the published latest.json is V's.

The website (rawtonic.cc, its own repository) builds its pages from release.json
with `node scripts/build.mjs --strict`, which refuses a whatsNew of another
version and page text naming an older release.
"""
import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

LANGS = ("en", "de")
MONTHS = ("January", "February", "March", "April", "May", "June", "July", "August",
          "September", "October", "November", "December")
SUMS = "SHA256SUMS.txt"


def main_files(version):
    """The four downloads the website offers, by release.json key."""
    return {
        "mac": f"Rawtonic_{version}_aarch64.dmg",
        "macIntel": f"Rawtonic_{version}_x86_64.dmg",
        "linux": f"Rawtonic_{version}_amd64.AppImage",
        "windows": f"Rawtonic_{version}_x64.msi",
    }


def short(version):
    """0.1.0-alpha.11.1 -> alpha.11.1; 1.2.0 stays 1.2.0 (the website's heading)."""
    return version.split("-", 1)[1] if "-" in version else version


def mib(size):
    """Bytes as the website shows them: MiB rounded half-up ("42 MiB", "0 MiB")."""
    return f"{(2 * size + 1024 * 1024) // (2 * 1024 * 1024)} MiB"


def release_date(changelog, version):
    m = re.search(rf"^## {re.escape(version)} — (\d{{4}})-(\d{{2}})-(\d{{2}})\s*$", changelog, re.M)
    if not m:
        raise ValueError(f"CHANGELOG.md has no '## {version} — YYYY-MM-DD' heading")
    y, mo, d = (int(x) for x in m.groups())
    if not 1 <= mo <= 12:
        raise ValueError(f"CHANGELOG.md: {version} has the date {m.group(0)}")
    return f"{y:04d}-{mo:02d}-{d:02d}", f"{d} {MONTHS[mo - 1]} {y}"


def read_sums(path):
    """{file: sha256} from a `shasum -a 256` list (text or binary mode)."""
    sums = {}
    for line in Path(path).read_text().splitlines():
        if not line.strip():
            continue
        m = re.fullmatch(r"([0-9a-f]{64}) [ *](.+)", line)
        if not m:
            raise ValueError(f"{path}: not a SHA-256 line: {line!r}")
        sums[m.group(2)] = m.group(1)
    return sums


def facts(version, changelog, assets):
    """release.json's release facts from the CHANGELOG and the release's assets."""
    assets = Path(assets)
    sums = read_sums(assets / SUMS)
    if SUMS in sums:
        raise ValueError(f"{SUMS} lists itself")
    # The sums file is a file of the release too; its own hash is computed.
    sums[SUMS] = hashlib.sha256((assets / SUMS).read_bytes()).hexdigest()

    def entry(name):
        f = assets / name
        if not f.is_file():
            raise ValueError(f"{SUMS} lists {name}, which is not in {assets}")
        return {"file": name, "size": mib(f.stat().st_size), "sha256": sums[name]}

    files = main_files(version)
    for name in files.values():
        if name not in sums:
            raise ValueError(f"{SUMS} has no {name}")
    date, date_text = release_date(changelog, version)
    out = {"version": version, "date": date, "dateText": date_text, "tag": f"v{version}"}
    out.update({key: entry(name) for key, name in files.items()})
    others = [n for n in sums if n not in files.values() and n != SUMS] + [SUMS]
    out["otherAssets"] = [entry(n) for n in others]
    return out


def merged(release, new_facts):
    """release.json with the facts replaced; every other key (whatsNew, _comment) kept.
    `name` (an earlier release's title, used by no page) is dropped so it cannot go stale."""
    out = {}
    for key, value in release.items():
        if key == "name":
            continue
        out[key] = new_facts.get(key, value)
    for key, value in new_facts.items():
        out.setdefault(key, value)
    return out


def whats_new_problems(release):
    """Why release.json's whatsNew is not this release's (the website's build checks the rest)."""
    w = release.get("whatsNew")
    version = release["version"]
    if not isinstance(w, dict):
        return ["release.json has no whatsNew"]
    if w.get("version") != version:
        return [f"whatsNew is for {w.get('version')}, not {version}"]
    return [f"whatsNew has no '{lang}' block" for lang in LANGS if not isinstance(w.get(lang), dict)]


def page_problems(html, version, lang):
    """What a fetched home page lacks: V in its download facts and its What's new heading."""
    problems = []
    heading = {"en": "New in", "de": "Neu in"}[lang]
    m = re.search(r'<h2[^>]*\bid="new-title"[^>]*>(.*?)</h2>', html, re.S)
    if not m:
        problems.append("no What's new heading (id=\"new-title\")")
    elif m.group(1).strip() != f"{heading} {short(version)}.":
        problems.append(f"What's new heading is {m.group(1).strip()!r}, not '{heading} {short(version)}.'")
    for key, name in main_files(version).items():
        url = f"/releases/download/v{version}/{name}"
        if url not in html:
            problems.append(f"no download link for {key} ({url})")
    if f"Rawtonic {version}" not in html:
        problems.append(f"the page does not say 'Rawtonic {version}'")
    return problems


def manifest_problems(text, version):
    try:
        m = json.loads(text)
    except ValueError as e:
        return [f"latest.json is not JSON: {e}"]
    problems = []
    if m.get("version") != version:
        problems.append(f"latest.json is for {m.get('version')}, not {version}")
    if not m.get("platforms"):
        problems.append("latest.json lists no platforms")
    return problems


def report(problems, ok):
    for p in problems:
        print(f"website: {p}", file=sys.stderr)
    if not problems:
        print(ok)
    return 1 if problems else 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    w = sub.add_parser("write")
    w.add_argument("--version", required=True)
    w.add_argument("--changelog", required=True)
    w.add_argument("--assets", required=True)
    w.add_argument("--release-json", required=True)
    p = sub.add_parser("verify-page")
    p.add_argument("--version", required=True)
    p.add_argument("--lang", required=True, choices=LANGS)
    p.add_argument("page")
    v = sub.add_parser("verify-manifest")
    v.add_argument("--version", required=True)
    v.add_argument("manifest")
    a = ap.parse_args(argv)

    if a.cmd == "write":
        path = Path(a.release_json)
        try:
            new = facts(a.version, Path(a.changelog).read_text(), a.assets)
        except ValueError as e:
            return report([str(e)], "")
        release = merged(json.loads(path.read_text()), new)
        tmp = path.with_name(path.name + ".tmp")
        tmp.write_text(json.dumps(release, ensure_ascii=False, indent=2) + "\n")
        tmp.replace(path)
        print(f"website: {path} has the facts of {a.version} ({4 + len(new['otherAssets'])} files)")
        problems = whats_new_problems(release)
        if problems:
            problems.append(f"write whatsNew ({', '.join(LANGS)}) for {a.version} with the CHANGELOG section "
                            "(docs/RELEASING.md), on the website branch the stage uses, then run the stage again")
        return report(problems, f"website: whatsNew is {a.version}'s")
    if a.cmd == "verify-page":
        return report(page_problems(Path(a.page).read_text(), a.version, a.lang),
                      f"website: the {a.lang} home page shows {a.version}")
    return report(manifest_problems(Path(a.manifest).read_text(), a.version), f"website: latest.json is {a.version}'s")


if __name__ == "__main__":
    sys.exit(main())
