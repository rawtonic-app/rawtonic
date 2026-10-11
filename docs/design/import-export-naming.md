# Import, export and naming

Reviewed the installed Capture One and Lightroom Classic applications, including their import and export dialogs, and Capture One's naming-format editor (11 October 2026).

## Comparison and decisions

| Reference | Observed pattern | Rawtonic change |
| --- | --- | --- |
| Capture One import | Separate source, destination, backup and naming tools; example destination | Source and destination together, separate naming column, backup section and selected-file summary |
| Capture One naming | Tokens mixed with custom text, example filename and saved presets | Shared naming editor with insert-at-cursor tokens, movable/removable blocks, free text, examples and locally saved presets |
| Capture One export | Checkable processing recipes, naming tool, output summary and fixed export action | Recipe sidebar with an explanation of which settings will run; independently scrolling settings and fixed action bar |
| Lightroom Classic import | Source on the left, selection in the middle, options on the right | Clear separation of input and output settings; retain Rawtonic's native file chooser |
| Lightroom Classic export | Preset library next to grouped settings; collapsible secondary sections | Wider dialog; collapsible sharpening, metadata and watermark sections |

## Scope and accuracy

- The editor uses the existing naming engines. Import and export still have different token syntax; only tokens implemented by each engine are offered.
- Import exposes filename, extension, padded sequence, session name and date tokens. Its engine currently uses source modification time for dates. Camera/EXIF tokens are not offered because ingest does not populate them.
- Examples use an illustrative date and the configured counter start, not a metadata-backed preview. The source filename is used when available. The explanatory note is in the format editor.
- Naming presets are stored locally, separately for import and export. Naming presets and export recipes also remember the counter start; older presets default to 1.
- Blocks can be reordered by dragging or with accessible left/right buttons, and removed. Palette tokens can be dragged before a block or to the final insertion target.
- Import and export accept an integer counter start from 0 to 4,294,967,295. Each operation starts there and increments by one; resumed exports preserve the original job index. Import exposes the same start through `{seq_start}`. Hot-folder ingest retains its existing counter behavior.
- Single-file exports with date tokens select a destination directory so the backend can resolve the source date, avoiding literal tokens in the output filename.
- Invalid token syntax and reserved filename characters are reported inline before starting.

## Sources

- [Capture One: File Naming](https://support.captureone.com/hc/en-us/sections/360000669658-File-Naming)
- [Capture One: Adopting tokens in naming dialogs](https://support.captureone.com/hc/en-us/articles/360002528857-Adopting-tokens-in-naming-dialogs)
- [Capture One: Create custom naming presets](https://support.captureone.com/hc/en-us/articles/360002529957-Create-custom-naming-presets)
- [Adobe: Filename Template Editor](https://helpx.adobe.com/lightroom-classic/desktop/import-photos/filename-template-editor-text-template.html)

## Verification

Native Rawtonic debug application with a disposable session in `/tmp/rawtonic-dialog-review`, using a copied repository image. Existing user photos were not imported or exported in the reference apps. Component tests cover export routing, recipe settings, naming edits, persistence and invalid tokens; utility tests cover the engines' differing date syntax. Native pointer tests also verify palette insertion and block reordering. Rust tests verify real ingest copies with a custom counter and export queue serialization/resume numbering. TypeScript, the production web build and targeted ESLint checks are also run.
