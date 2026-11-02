# Code signing policy

> **Status: prepared, not in effect.** Rawtonic's Windows downloads are **not code-signed yet**. This policy is written for signing through the [SignPath Foundation](https://signpath.org) programme for open-source projects and applies from the first release signed that way; the release notes will say which release that is. Until then, check downloads with their SHA-256 (below). The same policy is published at <https://rawtonic.cc/code-signing/>.

Once in effect: Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

## What is signed

- Only Windows files built from this public repository, [rawtonic-app/rawtonic](https://github.com/rawtonic-app/rawtonic), by its GitHub Actions workflow [`.github/workflows/windows-installer.yml`](../.github/workflows/windows-installer.yml) on GitHub-hosted runners, from a release commit on `main`. Nothing built on a developer's computer is signed.
- The files: the app `rawtonic-app.exe`, the installers `Rawtonic_<version>_x64.msi` and `Rawtonic_<version>_x64-setup.exe`, and the copy of `rawtonic-app.exe` in `Rawtonic_<version>_x64-portable.zip`.
- Third-party libraries shipped with the app (for example Microsoft's ONNX Runtime DLLs) are not signed by this project; they keep their makers' own signature, if any.
- Every signing request is approved by hand by an approver (below), for one release at a time.

## Team and roles

| Role | Who |
|---|---|
| Committers and reviewers (may change the source without further review) | Members of the [rawtonic-app organisation](https://github.com/orgs/rawtonic-app/people): [Niels Franke](https://github.com/nielsfranke) |
| Approvers (decide whether a release is signed) | Owners of the [rawtonic-app organisation](https://github.com/orgs/rawtonic-app/people?query=role%3Aowner): [Niels Franke](https://github.com/nielsfranke) |

Changes from anyone else (pull requests) are reviewed by a committer before they are merged. All team members use multi-factor authentication for GitHub and SignPath.

## How to check a download

- **Signature** (signed releases): right-click the file → **Properties** → **Digital Signatures**; the signer is **SignPath Foundation**. In PowerShell: `Get-AuthenticodeSignature .\Rawtonic_<version>_x64.msi` must show `Status: Valid` and a signer certificate issued to SignPath Foundation.
- **Checksum** (every release): `Get-FileHash .\<file>` must print the SHA-256 listed in the release's `SHA256SUMS-windows.txt`. This catches a damaged download; on its own it does not prove who made the file.
- If Windows or the browser holds a download back: [README → If Windows blocks the download](../README.md#if-windows-blocks-the-download).

## Privacy

Rawtonic sends nothing on its own except a startup update check: at most once every 24 hours it asks GitHub's public Releases API whether a newer version exists. It is on by default and can be turned off in Preferences → General ("Check for updates at startup"). Everything else that goes online (AI model downloads, maps, problem and crash reports, opt-in usage statistics) happens only when the user asks for it, after showing what is sent. Details: [PRIVACY.md](../PRIVACY.md) and <https://rawtonic.cc/privacy/>.

## Contact

Questions about this policy or a suspected misuse of a signature: hello@rawtonic.cc, or an [issue](https://github.com/rawtonic-app/rawtonic/issues).
