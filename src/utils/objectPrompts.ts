import { GEOMETRY_KEYS } from './geometryKeys';

/** An Object selection's prompts (the sub-mask's `prompts`, in the turned
 * photo's pixels like `startX`…`endY`): clicks and boxes; `exclude` takes
 * one out. Recipes from before keep only `startX`…`endY`, one box or a click
 * (`start == end`), and render from their stored mask as before. */
export type ObjectPrompt =
  | { type: 'point'; x: number; y: number; exclude?: boolean }
  | { type: 'box'; startX: number; startY: number; endX: number; endY: number; exclude?: boolean };

interface Coord {
  x: number;
  y: number;
}

/** What a press on the photo does to the prompts: a plain one starts over,
 * Shift adds an object, Alt (Option) takes a region out (the modifiers of
 * the object removal's Detect objects and of selection tools generally). */
export type PromptGesture = 'replace' | 'add' | 'exclude';

export function promptGesture(modifiers: { shiftKey?: boolean; altKey?: boolean }): PromptGesture {
  if (modifiers.altKey) return 'exclude';
  if (modifiers.shiftKey) return 'add';
  return 'replace';
}

/** The frame prompts are made in: the photo's turn (`rotation`, flips,
 * quarter turns) and the lens and transform warps before it that move
 * content (`GEOMETRY_KEYS`, stored with the mask as `warp`). Prompts of
 * another frame point elsewhere now. */
type Frame = Record<string, unknown>;

/** Values of a frame compared by content (object keys in any order). */
function canonical(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object')
    return Object.fromEntries(
      Object.keys(value as Record<string, unknown>)
        .sort()
        .map((key) => [key, canonical((value as Record<string, unknown>)[key])]),
    );
  return value;
}

/** Lens corrections that change the photo's tones or colour fringes but
 * move no content: prompts stay on their objects across them. */
const CONTENT_STAYS: ReadonlySet<string> = new Set([
  'lensVignetteAmount',
  'lensManualVignetteAmount',
  'lensManualVignetteMidpoint',
  'lensVignetteEnabled',
  'lensTcaAmount',
  'lensTcaEnabled',
  'autoChromaticAberration',
  'autoCaVersion',
  'fringeVersion',
]);

/** The lens and transform warps of `adjustments` that move content, as the
 * backend stores them in a mask's `warp` (unset ones left out). */
function warpOf(adjustments: Frame) {
  return Object.fromEntries(
    GEOMETRY_KEYS.filter((key) => !CONTENT_STAYS.has(key) && adjustments[key] != null).map((key) => [
      key,
      adjustments[key],
    ]),
  );
}

/** A key of the frame `adjustments` show the photo in: prompts made under
 * one key are valid under the same key only. */
export function promptFrameKey(adjustments: Frame): string {
  return JSON.stringify(
    canonical([
      Number(adjustments.rotation ?? 0),
      Boolean(adjustments.flipHorizontal),
      Boolean(adjustments.flipVertical),
      Number(adjustments.orientationSteps ?? 0),
      warpOf(adjustments),
    ]),
  );
}

/** The frame key of a mask's stored parameters (its turn and `warp`);
 * null for masks from before `warp` was stored. */
function storedFrameKey(params: Record<string, unknown>): string | null {
  if (!params.warp || typeof params.warp !== 'object') return null;
  return promptFrameKey({ ...params, ...(params.warp as Frame) });
}

/** A press from `start` to `end`: a click when it stays put. */
export function promptOf(start: Coord, end: Coord, exclude = false): ObjectPrompt {
  const prompt: ObjectPrompt =
    start.x === end.x && start.y === end.y
      ? { type: 'point', x: start.x, y: start.y }
      : { type: 'box', startX: start.x, startY: start.y, endX: end.x, endY: end.y };
  return exclude ? { ...prompt, exclude: true } : prompt;
}

/** The prompts a selection's mask was made from: its `prompts`, or the one
 * box or click of a recipe from before. None while it has no mask (new, or
 * pasted and waiting to be pointed at again) or when its frame changed
 * since (`promptsFrameChanged`). */
export function storedPrompts(params: Record<string, unknown> | undefined, adjustments: Frame): ObjectPrompt[] {
  if (!params?.maskDataBase64 || promptsFrameChanged(params, adjustments)) return [];
  if (Array.isArray(params.prompts)) return params.prompts as ObjectPrompt[];
  const { startX, startY, endX, endY } = params as Record<string, number | undefined>;
  if ([startX, startY, endX, endY].some((v) => typeof v !== 'number')) return [];
  return [promptOf({ x: startX!, y: startY! }, { x: endX!, y: endY! })];
}

/** Whether a selection's mask was made in another frame than the photo is
 * shown in now (turned, flipped, lens or transform changed; a mask from
 * before `warp` was stored counts as another): its prompts cannot be added
 * to, only started over. */
export function promptsFrameChanged(params: Record<string, unknown> | undefined, adjustments: Frame): boolean {
  return Boolean(params?.maskDataBase64) && storedFrameKey(params!) !== promptFrameKey(adjustments);
}

const includes = (prompts: ObjectPrompt[]) => prompts.some((p) => !p.exclude);

/** The prompts after a press: null when it changes nothing (taking out of
 * an empty selection). */
export function nextPrompts(current: ObjectPrompt[], prompt: ObjectPrompt, gesture: PromptGesture): ObjectPrompt[] | null {
  const { exclude: _exclude, ...included } = prompt;
  if (gesture === 'replace') return [included as ObjectPrompt];
  if (gesture === 'add') return [...current, included as ObjectPrompt];
  if (!includes(current)) return null;
  return [...current, { ...prompt, exclude: true }];
}

/** `startX`…`endY` of a prompt list (its first included prompt), so the
 * fields keep their meaning for readers that know only them. */
export function promptBounds(prompts: ObjectPrompt[]): { start: Coord; end: Coord } | null {
  const first = prompts.find((p) => !p.exclude);
  if (!first) return null;
  return first.type === 'point'
    ? { start: { x: first.x, y: first.y }, end: { x: first.x, y: first.y } }
    : { start: { x: first.startX, y: first.startY }, end: { x: first.endX, y: first.endY } };
}
