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

/** The photo's turn the prompts were made in (`rotation`, flips, quarter
 * turns): prompts of another turn point elsewhere now. */
interface Geometry {
  rotation?: number;
  flipHorizontal?: boolean;
  flipVertical?: boolean;
  orientationSteps?: number;
}

function sameGeometry(params: Record<string, unknown>, geometry: Geometry) {
  return (
    Number(params.rotation ?? 0) === (geometry.rotation ?? 0) &&
    Boolean(params.flipHorizontal) === Boolean(geometry.flipHorizontal) &&
    Boolean(params.flipVertical) === Boolean(geometry.flipVertical) &&
    Number(params.orientationSteps ?? 0) === (geometry.orientationSteps ?? 0)
  );
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
 * pasted and waiting to be pointed at again) or when the photo was turned
 * since. */
export function storedPrompts(params: Record<string, unknown> | undefined, geometry: Geometry): ObjectPrompt[] {
  if (!params?.maskDataBase64 || !sameGeometry(params, geometry)) return [];
  if (Array.isArray(params.prompts)) return params.prompts as ObjectPrompt[];
  const { startX, startY, endX, endY } = params as Record<string, number | undefined>;
  if ([startX, startY, endX, endY].some((v) => typeof v !== 'number')) return [];
  return [promptOf({ x: startX!, y: startY! }, { x: endX!, y: endY! })];
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
