# Changelog

## Unreleased

- Exports in Display P3, Adobe RGB (1998) and ProPhoto RGB, chosen in the export dialog and saved in recipes (`--color-space` for `rawtonic export` and the headless export). JPEG, PNG, TIFF, WebP and AVIF carry the matching ICC profile, JPEG XL declares the space.
- One wide-gamut rendering for every output (ADR 0010): colours beyond sRGB are kept out to the ProPhoto gamut and clipped only by the output, as in Camera Raw; the look's saturated colours were refitted against Camera Raw's ProPhoto renders (281 camera models; sRGB exports 0.937 → 0.908 ΔE from Camera Raw). sRGB exports change accordingly.
- Develop view in Display P3 on macOS, and soft proofing of an output colour space with a gamut warning (Proof in the histogram bar, Shift+S).
- AVIF exports carry XMP (rating, caption, keywords, and the marking of AI-generated areas).
- Saturated highlights, blues and the lightness of colours refitted against Camera Raw (sRGB 0.901 → 0.887 ΔE; yellow highlights 2.21 → 1.88).
- JPEG, PNG, TIFF and WebP sources in Display P3, Adobe RGB, ProPhoto or another embedded profile are read in their colour space (they were read as sRGB).
- Fix: Vibrance turned some saturated reds and oranges grey (in exports and in the Develop view).
- Adding photographs is all or nothing: a session's new files, Add (catalog) and imports from another program's catalog read each photograph's editor sidecar or XMP first and store the photograph with its edit and metadata in one transaction. A crash or full disk no longer leaves a photograph registered without them; the next try adds it complete. A photograph whose sidecar cannot be read is reported and left out instead of stopping the session from opening. `rawtonic add` brings sidecar edits and XMP metadata too.
- Libraries record import batches (database schema 4): each ingest, Add or import remembers the photographs it added, the groundwork for Previous Import.
- **Synchronize Folder…** (a folder's menu in Folders, and the library menu): finds photographs renamed, moved between folders, added or deleted outside Rawtonic, and lists them for review before anything changes. A move counts as verified only when the file's full SHA-256 equals the one on record; moves matched by size and sampled content only, copies of photographs in the library and removals start unticked, and files that could be several photographs need a choice. Photographs on a disconnected drive are never treated as moved or removed. Applying checks every change again and skips what changed since the scan; moved photographs keep their edits, ratings, keywords, versions and albums, and an XMP sidecar that moved along stays the library's. Files on disk are never moved, changed or deleted; a removal only removes the library entry. `rawtonic sync-folder LIBRARY [FOLDER] [--apply]` does the same without the app (`--include-probable`, `--include-copies`, `--remove-missing`).
- Every photograph gets its content hash (SHA-256, database schema 5) on record: ingest stores the hash it verified the copy with; others are hashed in the background after a library opens, one file at a time with pauses (`rawtonic checksums LIBRARY` hashes them at once).
- **Previous Import** in Library › Albums shows the photographs the latest ingest, Add or Synchronize Folder added.

- Rename the app from Re-touch to **Rawtonic** (rawtonic.cc). Bundle ID `cc.rawtonic.editor`; settings, presets, LUTs, albums and the remembered library are carried over once from `io.retouch.editor` and the former files stay untouched. CLI `rawtonic`, crates `rawtonic-*`, environment variables `RAWTONIC_*`.
- One export dialog: the Studio export now has WebP, JPEG XL and AVIF, resizing by long/short edge, width, height, megapixels or percentage with an optional enlarge, watermark, border, pad to aspect ratio, a subfolder for every destination, kept folder structure, capture-time file dates, mask export and an estimated size, all saved in recipes. The inherited export panel is removed; older recipes open with the previous defaults.
- XMP sidecars use the namespace `https://rawtonic.cc/ns/1.0/`; sidecars written as Re-touch are still read and converted on the next save. Sidecars no longer grow with every save (each rewrite used to leave an empty description behind).

## 0.1.0-dev — 2026-10-01

- Fix Brightness sensitivity by sharing the engine's −5…5 range and 0.01 steps across editing panels; preserve stored recipe units.
- Audit Develop tools: fix toolbar reset/paste event routing, crop-ratio application, incomplete effect reset, missing drag signaling and local/global tool context. Bound curve points and uploads to the GPU capacity; add control regression and pixel probes.

- Reduce measured RAW slider preview round trips from 176–184 ms to 8–10 ms on the development Mac at 2304×1728: specialize inactive shader work, cache needed blur tiles within 192 MiB, and keep thumbnail/export work from evicting preview inputs.
- Keep one active/latest pending preview, preserve the released value, remove the 50 ms final-render delay, and update drag analytics at 10 Hz with a complete final update. Preserve export precision and preview resolution.
- Add native GPU pixel regression/timing probes, queue and hook regressions, and a controlled RAW slider comparison. Shader specialization is display-only; masks/NR retain the general pipeline and uncommon variants compile off the render worker.
- Add persisted grid/filmstrip pick/reject badges and color indicators, P/X/U culling, atomic batch flag writes, ordered acknowledgements and stale-session protection.
- Combine flag/rating/color/search filters with contextual counts and a complete reset; clear invisible Library selections after filtering.
- Preserve initialized metadata on reopen and prevent stale adapter labels from undoing color-label edits or clears.
- Cache filename extraction, reuse the locale collator and avoid duplicate grouping. Record a 20,596-entry synthetic query comparison and five-camera full-resolution export regression.
- Validate 20,596 private RAW working copies and 15 full-resolution exports across Sony, Fuji and DJI, preserving every original and publishing numeric evidence only.
- Use embedded camera JPEGs for unedited browser thumbnails, replace obsolete viewport work, and load library EXIF only for explicit EXIF sorts/queries.
- Preserve library references for unchanged thumbnail metadata, bound decoded retention to 1 GiB, share no-op geometry buffers, and handle disabled/oversized cache entries safely.
- Add opt-in profiling and regression tests; document remaining first-open, export, zoom and total-memory budget failures.
- Integrate native macOS window controls into the 42px session bar, remove the duplicate 48px custom title row and rounded inset frame, and retain a draggable header with a fullscreen-aware control inset.
- Replace the inherited primary interface with original Library/Develop workspaces, tools-left and tools-right layouts, compact histogram/tool tabs, fixed viewer toolbar and status areas.
- Add a virtualized library/browser and filmstrip, folder navigation, history, rating/search/sort controls, point curves and layer creation using existing native processing adapters.
- Add responsive numeric controls with typed input, scrub labels, reset, coarse/fine keys and frame-coalesced preview updates; flush final values and contain input Escape.
- Add an original JPEG/TIFF/PNG export dialog and retain advanced processing panels in a separate tools dialog during migration.
- Fix reviewer-identified Tab focus interception, late-selection filmstrip scrolling and panel width overrides; add regression coverage.
- Lower the desktop minimum to 780 × 560; record native desktop/narrow captures, synthetic export verification and G67 for secondary UI and latency qualification.

- Start Re-touch from attributed RapidRAW editor and rendering snapshot.
- Record architecture, reference evidence, parity gates and development plan before implementation.
- Add portable session/catalog schema, SQLite WAL/FTS, immutable primary recipe history and managed editor adapter.
- Add strict naming, verified no-clobber dual ingest, cancellation, duplicate checks and standalone storage CLI.
- Add atomic preserving XMP, explicit external adoption/conflict handling and standard color-label conversion.
- Add session/create/ingest workspace, keyboard pick/reject, modal focus/shortcut containment and local-only startup.
- Gate unsafe inherited move/rename/trash/import and unsupported tethering; protect export/generated image outputs.
- Fix headless export failure exit status and duplicate managed XMP writes; make AI runtime optional.
- Add native regression/shader/fixture tests, React flow tests, CI/packaging skeletons and license inventory.
- Remediate vulnerable TLS/XML dependencies; record remaining advisory and production qualification blockers.
