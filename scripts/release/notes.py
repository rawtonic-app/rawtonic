#!/usr/bin/env python3
"""Release notes for a GitHub release (run by scripts/release.sh, stage notes).

    scripts/release/notes.py --version V --prev P --changelog CHANGELOG.md \
        --prev-body PREV_NOTES.md --sums SHA256SUMS.txt > notes.md

The text that changes per release comes from CHANGELOG.md's `## V — DATE`
section (its intro, Highlights, Changes to existing edits, Known gaps/limits);
the text that stays (warning, Install, the camera libraries' source, Feedback)
from the previous release's notes with the version swapped. The SHA-256 list
comes from SHA256SUMS.txt. The result is a draft: read it before publishing.
"""
import argparse
import re
import sys
from pathlib import Path

REPO_URL = "https://github.com/rawtonic-app/rawtonic"


def short(version):
    """0.1.0-alpha.11.1 -> alpha.11.1; 1.2.0 stays 1.2.0."""
    return version.split("-", 1)[1] if "-" in version else version


def same_version(version):
    """A pattern for VERSION itself, not a longer one (0.1.0-alpha.11 in 0.1.0-alpha.11.1)."""
    return re.escape(version) + r"(?![0-9]|\.[0-9])"


def changelog_section(changelog, version):
    m = re.search(rf"^## {re.escape(version)} — .*?\n(.*?)(?=^## |\Z)", changelog, re.S | re.M)
    if not m:
        raise ValueError(f"CHANGELOG.md has no '## {version} — DATE' section")
    return m.group(1).strip()


def subsections(section):
    """Intro text and {heading: text} of the ### subsections."""
    parts = re.split(r"^### (.+)\n", section, flags=re.M)
    intro = parts[0].strip()
    return intro, {parts[i].strip(): parts[i + 1].strip() for i in range(1, len(parts), 2)}


def body_sections(body):
    """The previous notes as [(heading or None, text)] split at '## ' headings."""
    parts = re.split(r"^## (.+)\n", body, flags=re.M)
    sections = [(None, parts[0])]
    sections += [(parts[i].strip(), parts[i + 1]) for i in range(1, len(parts), 2)]
    return sections


def notes(version, prev, changelog, prev_body, sums):
    section = changelog_section(changelog, version)
    intro, sub = subsections(section)
    if not intro:
        raise ValueError(f"the {version} section has no introduction")
    highlights = sub.get("Highlights")
    if not highlights:
        raise ValueError(f"the {version} section has no '### Highlights'")
    prev_sections = body_sections(prev_body.replace("\r\n", "\n"))
    head = prev_sections[0][1]
    warning = re.search(r"^> \[!WARNING\]\n(?:> .*\n?)+", head, re.M)
    if not warning:
        raise ValueError("the previous notes have no '> [!WARNING]' block")

    def kept(name):
        found = [(h, t) for h, t in prev_sections if h and h.startswith(name)]
        if not found:
            raise ValueError(f"the previous notes have no '## {name}…' section")
        heading, text = found[0]
        text = text.split("\n---\n")[0]
        return re.sub(same_version(prev), version, f"## {heading}\n{text}").strip()

    install = kept("Install")
    if prev != version and re.search(same_version(prev), install):
        raise ValueError("the previous version is left in the install text")
    names = {line.split()[1].lstrip("*"): line.split()[0] for line in sums.splitlines() if line.strip()}
    tail = [kept("Source code"), kept("Feedback")]
    tarballs = sorted(n for n in names if re.search(r"\.tar\.(bz2|xz|gz)$", n))
    for i, line in enumerate(tail):
        # The camera libraries' tarball names as attached to this release.
        for name in tarballs:
            stem = re.match(r"(lib[a-z0-9]+)-", name).group(1)
            line = re.sub(rf"`{stem}-[0-9][^`]*`", f"`{name}`", line)
        tail[i] = line

    out = [
        f"**Rawtonic {version}**: a free, open-source RAW developer for photographers.",
        warning.group(0).strip(),
        intro,
        f"Updating from {short(prev)}: replace the app (macOS: drag the new Rawtonic to Applications; Linux: "
        "AppImageUpdate or Gear Lever, or download the new AppImage; Windows: run the new installer). "
        "Your libraries, sessions and edits stay as they are.",
        install,
        f"## What's new since {short(prev)}\n\n{highlights}",
    ]
    if sub.get("Changes to existing edits"):
        out.append(f"## Changes to existing edits\n\n{sub['Changes to existing edits']}")
    limits = sub.get("Known gaps") or sub.get("Known limits")
    out.append(
        f"## Known limits\n\n{limits}" if limits else
        f"## Known limits\n\nThe known limits of {short(prev)} remain: "
        f"[{short(prev)} release notes]({REPO_URL}/releases/tag/v{prev})."
    )
    out.append(f"The technical details: [CHANGELOG.md]({REPO_URL}/blob/v{version}/CHANGELOG.md).")
    out += tail
    shas = ["---", "SHA-256 (also in `SHA256SUMS.txt`):", ""]
    for name in sorted(names):
        if name.startswith("Rawtonic_") or name == "SHA256SUMS-windows.txt":
            shas.append(f"- `{name}`: `{names[name]}`")
    if not any(n.endswith("_aarch64.dmg") for n in names):
        raise ValueError("SHA256SUMS.txt lists no Apple Silicon DMG")
    out.append("\n".join(shas))
    return "\n\n".join(out) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--version", required=True)
    parser.add_argument("--prev", required=True)
    parser.add_argument("--changelog", type=Path, required=True)
    parser.add_argument("--prev-body", type=Path, required=True)
    parser.add_argument("--sums", type=Path, required=True)
    args = parser.parse_args()
    try:
        text = notes(args.version, args.prev, args.changelog.read_text(), args.prev_body.read_text(),
                     args.sums.read_text())
    except (ValueError, OSError) as error:
        sys.exit(f"notes: {error}")
    sys.stdout.write(text)


if __name__ == "__main__":
    main()
