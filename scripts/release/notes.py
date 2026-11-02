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


WINDOWS_HELP = "https://github.com/rawtonic-app/rawtonic#if-windows-blocks-the-download"


def windows_install(version):
    """The Windows part of Install when the release has the MSI and the portable zip."""
    return f"""### Windows (experimental, unsigned)

Three downloads of the same app, for Windows 10 or 11 (x64):

- `Rawtonic_{version}_x64.msi`: **the recommended download.** Installs for all users into Program Files; Windows asks for administrator permission. Installed an earlier release with the `-setup.exe`? Uninstall that copy first in Settings → Apps (libraries, edits and settings stay); the MSI says so and stops otherwise.
- `Rawtonic_{version}_x64-setup.exe`: installs for your user only, without administrator rights.
- `Rawtonic_{version}_x64-portable.zip`: portable, no installation. Unpack it anywhere and start `Rawtonic\\rawtonic-app.exe`. It needs the Microsoft Edge WebView2 Runtime, which Windows 11 includes and Edge installs on Windows 10. Settings and libraries are kept in your user profile, not beside the app.

1. Download one of them and `SHA256SUMS-windows.txt` below.
2. **The downloads are not signed.** Check the download first: in PowerShell, `Get-FileHash .\\<file>` must print the same SHA-256 as `SHA256SUMS-windows.txt` (upper or lower case does not matter). This catches a damaged download; it does not prove who made the file.
3. Start it. Windows SmartScreen says "Windows protected your PC": click **More info**, then **Run anyway**. Uninstall an installed Rawtonic in Settings → Apps.
4. Installing takes about a minute: at the end the installer prepares the graphics pipelines, so the first photo appears sooner.

If your browser or Windows Security removes or blocks the download, see [Windows blocked the download?]({WINDOWS_HELP}). Each download was scanned with Microsoft Defender on a Windows build machine before the release; that is evidence, not a promise that your PC's protection agrees.

**Rawtonic needs a graphics card with a Vulkan or DirectX 12 driver.** PCs with only Microsoft's software renderer (WARP, for example a virtual machine without a graphics driver) are not supported: Rawtonic says so and closes instead of starting. **Windows support is experimental:** please [report]({REPO_URL}/issues) how it runs on your PC."""


def with_windows_install(install, version, names):
    """Install with its Windows subsection rewritten when the release has the MSI and the zip."""
    if not (f"Rawtonic_{version}_x64.msi" in names and f"Rawtonic_{version}_x64-portable.zip" in names):
        return install
    replaced, count = re.subn(r"^### Windows.*?(?=^### |\Z)", lambda _: windows_install(version) + "\n\n",
                              install + "\n", flags=re.S | re.M)
    if count != 1:
        raise ValueError("the install text has no '### Windows' subsection")
    return replaced.strip()


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
    install = with_windows_install(install, version, names)
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
        "AppImageUpdate or Gear Lever, or download the new AppImage; Windows: run the new installer, or unpack the new portable zip). "
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
