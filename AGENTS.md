# AGENTS.md

Guidance for coding agents (Claude Code, OpenAI Codex and others) working in this repository. It is the single instruction file: `CLAUDE.md` only imports it, so edit this file, never a copy.

## Rules

- **One tool per job.** For every tool and system, take the *better* approach of the leading professional RAW editors — decided by measurement and use — not everything from all of them. Never per-editor modes or "editor A / editor B" switches. Importers may translate another editor's settings into that one tool. The goal is the best RAW development tool, not a clone.
- **The bar is law.** Every tool and system must be equal to or better than the better of the leading professional RAW editors (quality, speed, workflow), shown by measurement or a documented side-by-side use test. A tool below that bar is not done: record the gap and the evidence, never call it finished.
- Rust 2024 (toolchain pinned to 1.98.1 in `rust-toolchain.toml`), Node 24+. AGPL-3.0-or-later for original additions; the editor derives from RapidRAW (commit `a82251f`), so preserve upstream copyright/license notices on inherited code.
- Never alter originals implicitly. The user's RAW files must never be deleted or modified; QA uses separate working copies. Every file write must use the atomic/no-clobber helpers (`rawtonic-metadata`, `rawtonic-ingest`, catalog). Unsupported destructive operations must fail clearly rather than fall back to deletion or overwrite. Tethering frees a photo from a camera's USB transfer queue (not a card file) only after its verified commit to the library (on unless the user turns it off, ADR 0012); test tools follow the same rule and never delete downloaded shots.
- No false completion claims or fake benchmark results. A budget is only green with measured evidence; label what a measurement does and does not cover (e.g. IPC round trip ≠ pointer-to-photon).
- `APP_FIXTURES` / `RAWTONIC_RAW_FIXTURES` point to private RAW fixtures — never commit photographs, private paths, manifests, raw app logs, credentials or keys. Keep `refs/` gitignored. Commits use a noreply address; the pre-push hook `scripts/git-hooks/pre-push` refuses other addresses and gitleaks findings.
- Conventional commits at green milestones; run `scripts/check.sh` before merging into main. Do not publish or tag a production release while parity or benchmark gates are open.
- Maintainers may keep extra, private instructions in `CLAUDE.local.md` (gitignored). Every agent reads it at the start of a session when present and follows it (Claude Code loads it automatically; other agents open it themselves).

## Working with several agents

Several agents may work on this repository, one after another or in parallel. Shared state lives in git, never only in one agent's private memory or chat:

- **Start of a session:** `git fetch`, `git status -sb`, `git worktree list`; read the private handoff notes named in `CLAUDE.local.md` when present. Do not touch a worktree or branch with uncommitted changes you did not make — another agent may be working there.
- **One agent per worktree.** Each piece of work gets its own branch and worktree (e.g. `../rawtonic-wt/<name>`); agents meet through commits and merges, not half-finished files.
- **Before stopping or handing over:** commit (a `wip:` commit is fine on a branch) and push the branch, and write down state, next step and open problems where `CLAUDE.local.md` says, so the next agent can continue without this chat.
- **Knowledge:** lessons, pitfalls, measurement methods and user decisions go into the shared notes named in `CLAUDE.local.md` (or this file, if they are public and general), not into an agent-specific memory.
- **Second-model review.** A different model reviews every change before it is merged: `scripts/ship.sh` runs `scripts/review.sh` (Codex, read-only) and merges only on `approve` for that exact HEAD; the verdict goes into the PR. Fix blocking findings and ship again; `--no-review REASON` only when the reviewer is unavailable or the change is docs-only, with the reason in the PR. Before a large work package (more than about a day, or several crates), write the plan and get `scripts/review.sh --plan FILE` to approve it before building. The review checks above all: special cases for one camera, file or test image instead of general solutions; parameters fitted to the evaluation set; changes spread over unrelated files or crates; version keys or compatibility layers that guard nothing; claims without measurements (criteria: `scripts/review/prompt.md`).
- **Enforcement** belongs in git hooks and `scripts/check.sh`, which every agent runs; agent-specific hooks or settings are only conveniences.

## Legal guardrails (check before implementing, so work is not redone)

- **No other products' content.** Never copy, convert or fit to other editors' or camera makers' profiles, DCP/ICC files, look tables, LUTs, presets, styles or curves (Adobe, Capture One, Fujifilm, …). Observing another program's *output* on our own test files to understand its behaviour is fine; the result must be our own implementation and our own numbers. Plain numerical camera facts (colour matrices, white levels, baseline exposure) are fine with their source stated in `THIRD_PARTY_NOTICES.md`. The same holds for published film and paper datasheet measurements (spectral sensitivity, characteristic curves, dye densities) as input to our own film model: source only in `THIRD_PARTY_NOTICES.md`, never ship the datasheets or redrawn graphs, never the product's name in the UI, docs or release texts.
- **No third-party names for our features.** Tools, looks, presets and profiles get descriptive names, never another company's product or trademark (no film-simulation, film-stock or editor-look names). Naming another product is fine only to say what we import from or work with (`TRADEMARKS.md`).
- **Licenses.** Code only under AGPL-3.0-compatible licenses (no GPL-2.0-only, no proprietary or decompiled code), with notices kept. New third-party code, data or assets go into `THIRD_PARTY_NOTICES.md` (and its copy in `src-tauri/resources/licenses/`, checked by `scripts/check.sh`) and the About page credits (`src/components/panel/AboutSettings.tsx`). Keep the DNG notice ("This product includes DNG technology under license by Adobe.") in About, NOTICE and README. AGPL means no Mac App Store distribution.
- **AI models.** Only weights with an explicit MIT/Apache-2.0/BSD-style license from the rights holder — no non-commercial, research-only, use-restricted (RAIL) or unlicensed weights. Host them in the model pack (`scripts/models/build_model_pack.py`, pinned SHA-256) and record them in `docs/MODELS.md` with license texts in `licenses/models/`.
- **Generated content.** Anything a generative model adds to a photo must be an AI patch (`aiPatches`) so exports carry the IPTC Digital Source Type marking (EU AI Act Art. 50; `has_generated_areas`, `export_metadata_with`). A new generative feature must keep that marking working.
- **Privacy.** No network access the user did not ask for: no telemetry, update checks or background uploads. Any new network access needs a user action and an entry in `PRIVACY.md`.

## Commands

```sh
npm ci
npm start                       # = tauri dev; full app. `npm run dev` serves UI only and cannot decode/render photos
npm run typecheck
npm run lint
npm test                        # vitest (jsdom), src/**/*.test.{ts,tsx}
npx vitest run src/hooks/useThumbnails.test.ts   # single frontend test file (-t "name" to filter)
npm run build
npm run i18n:check
scripts/check.sh --brief       # every gate before a merge into main; CI repeats it on Linux after the merge. --brief: one line per step, failures show their last 40 lines
scripts/ship.sh [--title T] [--body-file F] [--no-merge] [--keep-worktree] [--dry-run] [--no-review REASON]   # from a clean branch worktree: full check (skipped if it passed on this HEAD) → second-model review (scripts/review.sh; skipped if it approved this HEAD) → push → PR → merge --no-ff into main → delete remote branch; removes worktree + local branch only when called from outside it (never from inside it or an Orca worktree — agents: send your final report, then run the removal command from the last line). Last line: `ship: merged PR #N as <sha>; …` / `ship: FAILED …`
scripts/review.sh [--base REF] | --plan FILE   # second-model review (Codex, read-only) of REF..HEAD or of a plan; exit 0 = approve; verdict stored per HEAD for ship.sh
scripts/check.sh --brief --changed   # while working: only the gates/Rust packages (and dependents) the change touches; never a substitute for the full run. --quick skips Rust tests

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
npm run core:test               # core crates only: catalog, naming, ingest, metadata, fileops
cargo test -p rawtonic-catalog --test session <name>   # single core test
cargo test -p rawtonic-app <name>                     # single native/adapter test
cargo test -p rawtonic-engine <name>                  # develop model, masks, looks (also -p rawtonic-rawdecode / rawtonic-gpu)
cargo test -p rawtonic-gpu -- --ignored gpu_processing::preview_tests   # GPU equivalence tests (needs a GPU)
scripts/check_layering.sh       # engine/core crates must not reach Tauri
APP_FIXTURES=/path/to/private-fixtures cargo test -p rawtonic-app --test fixture_render --locked
cargo deny check licenses bans sources
npm run tauri -- build          # optimized app → target/release/bundle/
scripts/models/install_models.sh [--runtime] cc.rawtonic.qa   # AI models from the private pack (gh), see docs/MODELS.md
```

Gotcha: workspace `default-members` excludes `src-tauri`, so plain `cargo test`/`cargo build` only cover the core and engine crates (incl. `rawtonic-gpu`) and the CLI. Use `--workspace` or `-p rawtonic-app` for the desktop crate. Native SDK prerequisites (Xcode CLT / Ubuntu webkit2gtk etc.) are listed in README. The `fixture_render` harness prints SKIP without `APP_FIXTURES`; that is not a pass. `check.sh` runs Rust tests with cargo-nextest when installed (fixture_render excluded in `.config/nextest.toml`, run by cargo test with doc tests); always build with `--workspace`, since `-p` subsets resolve other features and rebuild dependencies. Advisories: merges fail on vulnerabilities/unsound crates, unmaintained ones are kept out by `[patch.crates-io]` copies in `vendor/` (remove each when upstream fixes it; never an ignore entry) — details in `docs/DEVELOPMENT.md` and ADR 0005. CLI (`cargo build -p rawtonic-cli`, `./target/debug/rawtonic …`, export byte-identical to the app): `docs/DEVELOPMENT.md`.

## Architecture

Two halves, mid-migration (ADR 0001). **Before changing a crate, read its part of the crate map in `docs/ARCHITECTURE.md`** (what each crate holds, app glue modules, test hooks); CLI details: `docs/DEVELOPMENT.md`.

- **Core and engine crates (`crates/`)**, desktop-independent with real tests: `catalog` (SQLite, authoritative for managed assets), `fileops` (journaled undo/redo rename/move/trash; `purge` is the only deletion), `metadata` (atomic XMP), `naming`, `ingest` (verified no-clobber copy), `denoise`, `faces`, `onnx` (every ONNX model loads through it), `merge`, `interop`, `tether` (camera tethering helper process, ADR 0012), `cli`; engine layers lowest first `runtime` → `rawdecode` → `engine` → `gpu` → `export` (ADR 0009). `scripts/check_layering.sh` fails if any reaches Tauri.
- **Where to edit:** develop logic → the crate; a Tauri command or `AppState` access → the same-named thin module in `src-tauri/src` (e.g. `camera_profile.rs` = `pub use rawtonic_engine::camera_profile::*` + commands). Engine/GPU functions take data, never `AppState`. Test-only helpers for another crate go behind a `test-support` feature. Don't add empty crates pretending a separation exists.
- **Desktop adapter (`src-tauri/`, crate `rawtonic-app`, lib `rapidraw_lib`):** Tauri commands, `AppState`, caches, inherited Tauri-bound modules (file_management, ai_*, inpainting, tagging); `camera_tethering` is the glue to the `tether` crate's helper process (ADR 0012). `lib.rs` is wiring only; no crate-root re-exports — import from the owning module. `session_bridge.rs` bridges `.rrdata` sidecars and the SQLite session.

**Rendering:** native wgpu presents *under a transparent webview*. Develop pixels never cross IPC — only renderer commands and small thumbnails do. Do not replace rendering with CSS/canvas filters or fabricated previews. The display path uses specialized shader variants (compiled lazily, general shader as fallback); CPU-readback exports use the general shader and must stay pixel-exact. Changes to the pipeline are validated with the GPU equivalence tests in `crates/gpu/src/gpu_processing_tests.rs` (`cargo test -p rawtonic-gpu -- --ignored gpu_processing::preview_tests`) and byte-identical full-res export comparisons against the previous commit.

**Frontend (`src/`)** — React 19 + zustand stores (`src/store/`) + Tailwind 4, i18next. `src/components/studio/` is the original Rawtonic Library/Develop workspace; `components/panel`, `modals`, `adjustments` etc. are inherited RapidRAW UI, some still reachable via a separate "tools" dialog pending migration. Tauri command names are centralized in the `Invokes` enum in `src/components/ui/AppProperties.tsx`; most backend interaction lives in `src/hooks/` (`useImageLoader`, `useImageProcessing`, `useThumbnails`, `useTauriListeners`, …).

**Session layout on disk:** `session.json`, `session.sqlite` (WAL), `Capture/`, `Selects/`, `Output/`, `Trash/`, `masks/`, `.cache/`. Asset paths are relative so closed sessions can be moved. Catalog SQLite connections are never shared across threads; catalog work runs in blocking workers for IPC.

## Key docs

`docs/ARCHITECTURE.md`, `docs/DEVELOPMENT.md`, `docs/RECIPE_SCHEMA.md`, `docs/MODELS.md` (AI models, origins and licenses), `docs/testing/README.md` (fixture manifest format), `docs/decisions/` (ADRs), `CONTRIBUTING.md`, `PRIVACY.md`, `TRADEMARKS.md`, `THIRD_PARTY_NOTICES.md`.
