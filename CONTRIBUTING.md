# Contributing

Thanks for helping. Rawtonic aims to be the best RAW developer for
professional photographers; changes are measured, not guessed (see the
development guide in `docs/DEVELOPMENT.md` and the gates in
`scripts/check.sh`).

## License and Contributor License Agreement

Rawtonic is licensed under the GNU AGPL v3 or later. Contributions are
accepted under the [Contributor License Agreement](CLA.md): you keep the
copyright in your work and grant the maintainer the right to distribute it
under the AGPL and under other terms. By opening a pull request you agree
to the CLA for that and all future contributions.

## Bugs and ideas

Please [open an issue](https://github.com/rawtonic-app/rawtonic/issues/new)
rather than sending an e-mail. Say which Rawtonic version you run
(Preferences → About), your macOS version and Mac model, and for files
that do not open or look wrong, the camera model and file type (for
example Sony `.ARW`). A screenshot helps. Please don't attach private
photographs you don't want to be public; a description or a crop is
usually enough.

## What not to contribute

- **Code you may not relicense.** Only code under licenses compatible with
  the AGPL v3 (MIT, BSD, Apache-2.0, LGPL, GPL v3, AGPL v3, …), with its
  notices kept. No code from proprietary software, decompiled binaries or
  leaked sources, and no GPL-2.0-only code.
- **Other products' data.** No camera profiles, DCP/ICC files, LUTs, looks,
  presets, styles or curves copied, converted or fitted from other
  editors' or camera makers' files (Adobe, Capture One, Fujifilm and
  others). Behaviour may be compared by observing an editor's output on
  your own test files; the result must be Rawtonic's own implementation.
  Plain numerical camera facts (colour matrices, white levels, baseline
  exposure) are fine with their source stated.
- **Other companies' names** for Rawtonic's own tools, looks or presets
  (see `TRADEMARKS.md`).
- **AI models** without a clear MIT/Apache-2.0-style license from their
  rights holder, or under non-commercial or use-restricted terms. Record
  every model in `docs/MODELS.md`.
- **Personal data.** No photographs (unless you own them and license them
  for the repository), private paths, logs, keys or tokens. A pre-push
  hook runs gitleaks (`scripts/git-hooks/pre-push`).
- **Network access** the user did not ask for. Rawtonic does not phone
  home (see `PRIVACY.md`).
