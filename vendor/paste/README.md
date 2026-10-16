# paste → pastey stand-in

Rawtonic does not use `paste` itself. pulp 0.22.3 (through image → exr) still
depends on it, and `paste` is unmaintained (RUSTSEC-2024-0436). This crate
takes `paste`'s name and version through `[patch.crates-io]` in the workspace
`Cargo.toml` and re-exports the macros of `pastey` 0.2, the maintained fork of
`paste` (MIT OR Apache-2.0; both license texts are kept here). No code of
`paste` is used. Remove this patch when pulp releases with pastey (its main
branch already depends on `pastey = "0.2"`). See ADR 0005.
