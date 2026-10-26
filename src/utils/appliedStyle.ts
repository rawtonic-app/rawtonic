/**
 * The style last applied to a photograph, kept in its recipe so that the
 * next style replaces it and clicking it again removes it, also after
 * switching photos and restarting (crates/engine/src/applied_style.rs takes
 * styles back by the same rules on the photos of a selection; both check
 * the cases in `applied_style_cases.json`).
 *
 * `changes` are the settings the style sets (also those it leaves as they
 * were), at the deepest level of nested settings (one HSL or grading
 * slider, also inside a group the recipe did not have yet); lists (curve
 * points) count as one setting. `from` is the value before the style, `to`
 * the value it set, `full` its value at full strength once the Amount
 * changed; a missing one means the recipe did not have the setting. Taking
 * the style back sets each to `from`, but only where the recipe still holds
 * `to`: a setting the user changed after the style keeps the user's value
 * (it is the user's from then on, also for the Amount), and the user's
 * changes to other settings stay. A group the style added that ends up
 * empty goes too. Settings the app works out or rewrites per photo
 * (`PHOTO_OWN_KEYS`: the lens choice and its data, layers, relight lights)
 * are the photo's own: a style sets them, but they are not recorded. The
 * renderer never reads the key.
 */
export const APPLIED_STYLE = 'appliedStyle';

export interface StyleChange {
  path: string[];
  from?: unknown;
  to?: unknown;
  full?: unknown;
}

export interface AppliedStyle {
  id: string;
  /** Lightroom's Amount, in percent. */
  amount: number;
  changes: StyleChange[];
}

type Recipe = Record<string, unknown>;

const isGroup = (value: unknown): value is Recipe =>
  Boolean(value) && typeof value === 'object' && !Array.isArray(value);

function contains(current: unknown, to: unknown): boolean {
  if (typeof current === 'number' && typeof to === 'number') return current === to;
  if (Array.isArray(current) && Array.isArray(to))
    return current.length === to.length && to.every((value, i) => contains(current[i], value));
  if (isGroup(current) && isGroup(to))
    return [...Object.keys(current), ...Object.keys(to)].every((key) => holds(current[key], to[key]));
  return current === to;
}

/** Whether `current` still holds what a style set (`to`), exactly; a
 * missing setting and `null` are the same. */
function holds(current: unknown, to: unknown): boolean {
  if (current == null || to == null) return current == null && to == null;
  return contains(current, to);
}

function get(recipe: unknown, path: string[]): unknown {
  return path.reduce<unknown>((value, key) => (isGroup(value) ? value[key] : undefined), recipe);
}

/** `recipe` with the setting at `path` set (or with `undefined` removed,
 * and a group that leaves empty), copying the groups above it and making
 * missing ones; unchanged when one of them is not a group. */
function set(recipe: Recipe, path: string[], value: unknown): Recipe {
  const [key, ...rest] = path;
  const out = { ...recipe };
  if (rest.length === 0) {
    if (value === undefined) delete out[key];
    else out[key] = structuredClone(value);
    return out;
  }
  if (out[key] === undefined && value !== undefined) out[key] = {};
  if (!isGroup(out[key])) return recipe;
  const group = set(out[key] as Recipe, rest, value);
  if (value === undefined && Object.keys(group).length === 0) delete out[key];
  else out[key] = group;
  return out;
}

const changePath = (change: StyleChange) =>
  Array.isArray(change.path) && change.path.length > 0 && change.path.every((key) => typeof key === 'string')
    ? change.path
    : null;

/** Whether `recipe` still holds the value `change` set (else the user changed it). */
function stillHolds(recipe: unknown, change: StyleChange, path: string[]): boolean {
  return holds(get(recipe, path), change.to);
}

export function appliedStyleOf(recipe: unknown): AppliedStyle | null {
  const applied = isGroup(recipe) ? recipe[APPLIED_STYLE] : null;
  return isGroup(applied) && typeof applied.id === 'string' ? (applied as unknown as AppliedStyle) : null;
}

/** `recipe` with its last applied style taken back (see above), without the
 * `appliedStyle` key; `kept`: the paths of the style's settings the user
 * changed since, kept as the user set them; `removed`: whether a setting
 * went back to not being in the recipe. */
export function revertStyle<T extends object>(recipe: T): { recipe: T; kept: string[][]; removed: boolean } {
  const applied = appliedStyleOf(recipe);
  if (!applied) return { recipe, kept: [], removed: false };
  let out = { ...(recipe as Recipe) };
  delete out[APPLIED_STYLE];
  const kept: string[][] = [];
  let removed = false;
  for (const change of applied.changes ?? []) {
    const path = changePath(change);
    if (!path) continue;
    if (stillHolds(out, change, path)) {
      out = set(out, path, 'from' in change ? change.from : undefined);
      if (!('from' in change)) removed = true;
    } else kept.push(path);
  }
  return { recipe: out as T, kept, removed };
}

/** Whether a style's `value` for the setting at `path` is a group of
 * settings that goes setting by setting (into `own`, the photo's value):
 * not the layers, and not over a setting that is no group. */
const goesBySetting = (path: string[], value: unknown, own: unknown) =>
  isGroup(value) && !(path.length === 1 && path[0] === 'masks') && (own === undefined || isGroup(own));

/** `recipe` with a style's settings (`values`) applied: groups of settings
 * join the photo's (a style that sets one HSL slider leaves the others as
 * they were), every other value replaces it (`applied_style::merged`). */
export function withStyleValues<T extends object>(recipe: T, values: object): T {
  const merged = (path: string[], own: unknown, value: unknown): unknown => {
    if (!goesBySetting(path, value, own)) return value;
    const out: Recipe = { ...((own as Recipe | undefined) ?? {}) };
    for (const [key, next] of Object.entries(value as Recipe)) out[key] = merged([...path, key], out[key], next);
    return out;
  };
  const out: Recipe = { ...(recipe as Recipe) };
  for (const [key, value] of Object.entries(values)) out[key] = merged([key], out[key], value);
  return out as T;
}

function walk(values: Recipe, before: object, after: object, path: string[], out: StyleChange[]) {
  for (const [key, value] of Object.entries(values)) {
    if (path.length === 0 && key === APPLIED_STYLE) continue;
    const at = [...path, key];
    const [b, a] = [get(before, at), get(after, at)];
    if (goesBySetting(at, value, b) && isGroup(a)) walk(value as Recipe, before, after, at, out);
    // Every setting the style sets, also one it leaves as it was: a user
    // change to it is the user's, and its Amount still moves it.
    else if (b !== undefined || a !== undefined)
      out.push({
        path: at,
        ...(b !== undefined ? { from: structuredClone(b) } : {}),
        ...(a !== undefined ? { to: structuredClone(a) } : {}),
      });
  }
}

/** Settings the app works out or rewrites for each photo after a style set
 * them: the lens choice and its data (resolved per photo), layers (AI
 * detections, placed gradients, frames after a turn) and relight lights
 * (filled in on loading). They are the photo's own: a style sets them, but
 * its record leaves them out, so taking it back or replacing it leaves them
 * as they are (`applied_style::PHOTO_OWN_KEYS`). */
export const PHOTO_OWN_KEYS = [
  'lensCorrectionMode',
  'lensMaker',
  'lensModel',
  'cameraCaVersion',
  'cameraGeometryVersion',
  'lensDistortionParams',
  'lensProfileFor',
  'masks',
  'relightLights',
];

const withoutPhotoOwn = (values: object): Recipe =>
  Object.fromEntries(Object.entries(values).filter(([key]) => !PHOTO_OWN_KEYS.includes(key)));

/** `after` with the record that the style `id` (at `amount` percent) set
 * its settings (`values`, one by one inside groups) from their values in
 * `before` (the recipe with the previous style taken back) to those in
 * `after`. A style that changed nothing is recorded too: it shows as
 * applied, and a click removes it. */
export function recordStyle<T extends object>(before: object, after: T, values: object, id: string, amount = 100): T {
  const changes: StyleChange[] = [];
  walk(withoutPhotoOwn(values), before, after, [], changes);
  return { ...after, [APPLIED_STYLE]: { id, amount, changes } satisfies AppliedStyle };
}

/** Lightroom's Amount for one setting (`blendPreset`): numbers move part of
 * the way, other settings, curves and the `…Version` keys take the style's
 * value from half strength on. */
function blendSetting(path: string[], from: unknown, full: unknown, a: number): unknown {
  const whole = path[0] === 'curves' || path[0].endsWith('Version');
  if (!whole && typeof from === 'number' && typeof full === 'number') return from + a * (full - from);
  return a >= 0.5 ? full : from;
}

/** `recipe` with its applied style at `amount` percent: each setting still
 * the style's blended from its value before the style (in `base`: the
 * recipe without it, with the editor's defaults filled in; `defaults` for a
 * slider missing inside a group) to its value at full strength on this photo (as
 * recorded: `full`, or `to` at 100 %; else the style's own, `values`).
 * Settings the user changed since stay the user's and leave the record. */
export function withStyleAmount<T extends object>(
  recipe: T,
  base: object,
  values: object,
  amount: number,
  defaults: object = {},
): T {
  const applied = appliedStyleOf(recipe);
  if (!applied) return recipe;
  const a = Math.min(100, Math.max(0, amount)) / 100;
  let out = recipe as Recipe;
  const changes: StyleChange[] = [];
  for (const change of applied.changes ?? []) {
    const path = changePath(change);
    if (!path || !stillHolds(recipe, change, path)) continue;
    // Its value at full strength as recorded on this photo, else the style's.
    const full = 'full' in change ? change.full : applied.amount >= 100 ? change.to : get(values, path);
    const own = get(base, path);
    // A slider missing inside a group the photo has only partly blends from
    // its default; a missing setting of its own (a version key: the older
    // response) stays missing below half strength.
    const from = own !== undefined || path.length === 1 ? own : get(defaults, path);
    const to = blendSetting(path, from, full, a);
    out = set(out, path, to);
    const { to: _old, full: _full, ...rest } = change;
    changes.push({
      ...rest,
      ...(to !== undefined ? { to: structuredClone(to) } : {}),
      ...(full !== undefined ? { full: structuredClone(full) } : {}),
    });
  }
  return { ...out, [APPLIED_STYLE]: { ...applied, amount, changes } satisfies AppliedStyle } as T;
}
