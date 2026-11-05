// Contract of the commands converted to typed errors (architecture cleanup
// plan, step 1.2): a command that rejects with `CommandError` keeps the
// arguments and the result type it had while it rejected with a string, and
// the codes the frontend reads are the ones Rust sends. A command converted
// later is added to CONVERTED with its signature as it was before.
// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { Invokes } from '../components/ui/AppProperties';
import type { ErrorCode } from '../shared/ipc/errors';

const read = (path: string): string => readFileSync(path, 'utf8') as string;

const rustFiles = (dir: string): string[] =>
  (readdirSync(dir) as string[]).flatMap((name) => {
    const path = `${dir}/${name}`;
    if ((statSync(path) as { isDirectory(): boolean }).isDirectory()) return rustFiles(path);
    return path.endsWith('.rs') ? [path] : [];
  });

const squash = (text: string) =>
  text
    .replace(/\s+/g, ' ')
    .replace(/,\s*$/, '')
    .replace(/\s*,\s*\)/g, ')')
    .trim();

/** Every Tauri command of src-tauri: its arguments and its return type.
 * The argument list is read by parenthesis depth, not up to the first `)`:
 * a command with no return value and no `async` (or `#[allow]` between its
 * attributes) has a body whose grouping would otherwise pass for the list. */
function commands(): Map<string, { args: string; returns: string }> {
  const found = new Map<string, { args: string; returns: string }>();
  const signature =
    /#\[tauri::command[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*pub(?:\([a-z]+\))?\s+(?:async\s+)?fn\s+(?:<[^>]*>\s*)?([a-z0-9_]+)\s*(?:<[^>]*>)?\(/g;
  for (const file of rustFiles('src-tauri/src')) {
    const source = read(file);
    for (const signatureStart of source.matchAll(signature)) {
      const name = signatureStart[1];
      const open = signatureStart.index! + signatureStart[0].length;
      let depth = 1;
      let at = open;
      for (; at < source.length && depth > 0; at++) {
        // A comment's line is not part of the signature.
        if (source.startsWith('//', at)) {
          at = source.indexOf('\n', at);
          continue;
        }
        if (source[at] === '(') depth++;
        else if (source[at] === ')') depth--;
      }
      if (depth !== 0) continue;
      const rest = source.slice(at).match(/^\s*->\s*([^{]*?)\s*\{\n/);
      if (!rest) continue;
      found.set(name, {
        args: squash(source.slice(open, at - 1).replace(/\/\/[^\n]*/g, '')),
        returns: squash(rest[1])
      });
    }
  }
  return found;
}

/** Each converted command's arguments and result type, as they were while
 * it rejected with a `String` (main at d01b7add). */
const CONVERTED: Record<string, { args: string; ok: string }> = {
  analyze_face_retouch: {
    args: `js_adjustments: serde_json::Value, download: Option<bool>, state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle`,
    ok: `serde_json::Value`
  },
  apply_adjustments: {
    args: `preview_channel: Option<tauri::ipc::JavaScriptChannelId>, webview: tauri::Webview, js_adjustments: Option<serde_json::Value>, live_stroke: Option<crate::live_stroke::LiveStroke>, is_interactive: bool, target_resolution: Option<u32>, roi: Option<(f32, f32, f32, f32)>, request_analytics: bool, compute_waveform: bool, active_waveform_channel: Option<String>, sequence: Option<u64>, mask_overlay: Option<crate::app_state::MaskOverlayRequest>, crop_edit: Option<bool>, browser_preview_uri: Option<bool>, state: tauri::State<'_, AppState>`,
    ok: `Response`
  },
  duplicates_confirm: {
    args: `directory: String, keep: Vec<String>, trash: Vec<String>`,
    ok: `Vec<Refused>`
  },
  duplicates_groups: {
    args: `app: tauri::AppHandle, directory: String`,
    ok: `Vec<Vec<ReviewMember>>`
  },
  duplicates_start: {
    args: `app: tauri::AppHandle, directory: String`,
    ok: `u64`
  },
  apply_ai_denoise: {
    args: `path: String, amount: f32, version: Option<u8>, multiscale: Option<bool>, dead_pixel_adjacent: Option<bool>, unbiased_black: Option<bool>, border_black: Option<bool>, highlight_ratios: Option<bool>, state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle`,
    ok: `f32`
  },
  calculate_auto_straighten: {
    args: `path: String, adjustments: serde_json::Value, app_handle: tauri::AppHandle`,
    ok: `Option<AutoStraightenResult>`
  },
  detect_people: {
    args: `js_adjustments: serde_json::Value, state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle`,
    ok: `Vec<PersonSummary>`
  },
  edit_with_start: {
    args: `paths: Vec<String>, mut recipe: ExportRequest, program: String, app: AppHandle`,
    ok: `EditWithStart`
  },
  folder_sync_scan: {
    args: `directory: String, folder: Option<String>, app: tauri::AppHandle`,
    ok: `SyncPlan`
  },
  generate_ai_depth_mask: {
    args: `js_adjustments: serde_json::Value, path: String, min_depth: f32, max_depth: f32, min_fade: f32, max_fade: f32, feather: f32, rotation: f32, flip_horizontal: bool, flip_vertical: bool, orientation_steps: u8, task_id: Option<String>, state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle`,
    ok: `AiDepthMaskParameters`
  },
  generate_ai_foreground_mask: {
    args: `js_adjustments: serde_json::Value, path: String, rotation: f32, flip_horizontal: bool, flip_vertical: bool, orientation_steps: u8, subject_version: Option<u32>, task_id: Option<String>, state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle`,
    ok: `AiForegroundMaskParameters`
  },
  generate_ai_people_mask: {
    args: `js_adjustments: serde_json::Value, parts: Vec<String>, person: Option<usize>, rotation: f32, flip_horizontal: bool, flip_vertical: bool, orientation_steps: u8, task_id: Option<String>, state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle`,
    ok: `AiPeopleMaskParameters`
  },
  generate_ai_sky_mask: {
    args: `js_adjustments: serde_json::Value, path: String, rotation: f32, flip_horizontal: bool, flip_vertical: bool, orientation_steps: u8, task_id: Option<String>, state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle`,
    ok: `AiSkyMaskParameters`
  },
  generate_ai_subject_mask: {
    args: `js_adjustments: serde_json::Value, path: String, start_point: (f64, f64), end_point: (f64, f64), prompts: Option<Vec<ObjectPrompt>>, rotation: f32, flip_horizontal: bool, flip_vertical: bool, orientation_steps: u8, task_id: Option<String>, state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle`,
    ok: `AiSubjectMaskParameters`
  },
  invoke_generative_replace_with_mask_def: {
    args: `patch_definition: AiPatchDefinition, current_adjustments: Value, task_id: Option<String>, app_handle: tauri::AppHandle, state: tauri::State<'_, AppState>`,
    ok: `String`
  },
  library_export_catalog: {
    args: `source: String, paths: Vec<String>, destination: String, include_originals: bool, include_previews: bool, leave_out_owned_elsewhere: bool, app: tauri::AppHandle`,
    ok: `Receipt`
  },
  library_import_catalog: {
    args: `target: String, other: String, options: rawtonic_ingest::catalog_import::Options, app: tauri::AppHandle`,
    ok: `rawtonic_ingest::catalog_import::Receipt`
  },
  move_session: {
    args: `session: String, destination: String, app: AppHandle`,
    ok: `OperationSummary`
  },
  precompute_ai_subject_mask: {
    args: `js_adjustments: serde_json::Value, path: String, background: Option<bool>, task_id: Option<String>, state: tauri::State<'_, AppState>, app_handle: tauri::AppHandle`,
    ok: `()`
  },
  render_split_before: {
    args: `path: String, js_adjustments: serde_json::Value, target_resolution: Option<u32>, app_handle: tauri::AppHandle`,
    ok: `bool`
  },
  start_background_indexing: {
    args: `folder_path: String, app_handle: AppHandle, state: State<'_, AppState>`,
    ok: `()`
  },
  stacks_auto_apply: {
    args: `library: String, groups: Vec<AutoGroup>`,
    ok: `StackResult`
  },
  stacks_auto_plan: {
    args: `paths: Vec<String>, options: AutoStackOptions`,
    ok: `WebAutoPlan`
  },
  stacks_group: {
    args: `paths: Vec<String>`,
    ok: `StackResult`
  },
  stacks_list: {
    args: `directory: String`,
    ok: `Vec<WebStack>`
  },
  stacks_move: {
    args: `paths: Vec<String>, up: bool`,
    ok: `StackResult`
  },
  stacks_move_to_top: {
    args: `path: String`,
    ok: `StackResult`
  },
  stacks_remove: {
    args: `paths: Vec<String>`,
    ok: `StackResult`
  },
  stacks_restore: {
    args: `library: String, change: StackChange, undo: bool`,
    ok: `()`
  },
  stacks_set_collapsed: {
    args: `paths: Vec<String>, collapsed: Option<bool>`,
    ok: `StackResult`
  },
  stacks_split: {
    args: `paths: Vec<String>`,
    ok: `SplitResult`
  },
  stacks_unstack: {
    args: `paths: Vec<String>`,
    ok: `StackResult`
  },
  start_eye_check: {
    args: `directory: String, download: bool, resume: bool, app_handle: tauri::AppHandle`,
    ok: `()`
  }
};

describe('typed command errors', () => {
  const all = commands();
  const typed = [...all].filter(([, { returns }]) => /CommandError>$/.test(returns));

  it('converted commands keep their arguments and result types', () => {
    expect(all.size).toBeGreaterThan(300);
    expect(typed.map(([name]) => name).sort()).toEqual(Object.keys(CONVERTED).sort());
    for (const [name, { args, returns }] of typed) {
      expect({ name, args }).toEqual({ name, args: squash(CONVERTED[name].args) });
      expect({ name, returns }).toEqual({
        name,
        returns: expect.stringMatching(
          new RegExp(
            `^Result<${CONVERTED[name].ok.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}, (crate::ipc::)?CommandError>$`
          )
        )
      });
    }
    // Each is a command the frontend can name.
    const invoked = new Set(Object.values(Invokes) as string[]);
    expect(Object.keys(CONVERTED).filter((name) => !invoked.has(name))).toEqual([]);
  });

  it('the frontend knows exactly the codes Rust sends', () => {
    const rust = read('src-tauri/src/ipc/error.rs');
    const start = rust.indexOf('pub enum ErrorCode {');
    const body = rust.slice(start, rust.indexOf('\n}', start));
    const snake = (name: string) => name.replace(/[A-Z]/g, (c, i: number) => (i ? '_' : '') + c.toLowerCase());
    const rustCodes = [...body.matchAll(/^\s{4}([A-Z][A-Za-z]+),$/gm)].map(([, name]) => snake(name)).sort();
    const ts = read('src/shared/ipc/errors.ts');
    const union = ts.slice(
      ts.indexOf('export type ErrorCode ='),
      ts.indexOf(';', ts.indexOf('export type ErrorCode ='))
    );
    const tsCodes = [...union.matchAll(/'([a-z_]+)'/g)].map(([, code]) => code).sort();
    expect(rustCodes.length).toBeGreaterThan(8);
    expect(tsCodes).toEqual(rustCodes);
    // The type stays tied to the list read here.
    const sample: ErrorCode = 'cancelled';
    expect(tsCodes).toContain(sample);
  });
});
