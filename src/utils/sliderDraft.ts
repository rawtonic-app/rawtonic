import { useSyncExternalStore } from 'react';
import type { Adjustments } from './adjustments';

/**
 * Slider drags render every move but commit to the editor store at most every
 * COMMIT_INTERVAL_MS: each store commit re-renders the Develop panel, the
 * viewer and the workspace (on Linux's WebKitGTK ~9 ms per move, which held
 * the webview to 8–13 frames per second during a drag). The preview pipeline
 * registers a renderer for the moves in between; the draft is committed
 * (with its history step) on the interval, before any other edit, and when
 * the drag ends, so the store ends with exactly the last move.
 *
 * 250 ms (4 Hz): displays linked to the dragged value (colour-wheel puck,
 * refinement enable states) follow at that rate unless they read the draft
 * (`useSliderDraft`, as the region curve under the tone curve does); the
 * photo and the dragged control itself follow every move. Of
 * Develop's tools only the dragged one re-renders on these commits, the
 * others when the drag ends (ToolColumn in toolLayout.tsx). At
 * 100 ms these commits were nearly all of the ~95 ms React per 1 s drag on
 * Linux (user decision 2026-10-18, option B; notes: parity-gaps/G55).
 */
export const COMMIT_INTERVAL_MS = 250;

/** Renders a drag's uncommitted adjustments; false if it cannot (then the
 * update commits at once, as without drafts). */
type DraftRenderer = (path: string, adjustments: Adjustments) => boolean;
let renderer: DraftRenderer | null = null;

export function registerSliderDraftRenderer(next: DraftRenderer): () => void {
  renderer = next;
  return () => {
    if (renderer === next) renderer = null;
  };
}

let draft: { path: string; adjustments: Adjustments } | null = null;
let commitTimer: ReturnType<typeof setTimeout> | null = null;
let committer: ((path: string, adjustments: Adjustments) => void) | null = null;
/** The adjustments last sent to the renderer: the committed copy needs no
 * second render. */
let lastRendered: Adjustments | null = null;

/** Called when the draft changes (a move, its commit). */
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((listener) => listener());

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** `select` of the drag's newest adjustments for `path` (null without a
 * draft), re-rendering on each move that changes it: for displays linked to
 * the dragged value that should follow the drag, not the commits. `select`
 * must return a part of the adjustments, not a new object. */
export function useSliderDraft<T>(path: string | undefined, select: (adjustments: Adjustments) => T): T | null {
  return useSyncExternalStore(subscribe, () => {
    const adjustments = path ? draftFor(path) : null;
    return adjustments ? select(adjustments) : null;
  });
}

/** The drag's newest adjustments for `path`, if a draft is pending. */
export function draftFor(path: string): Adjustments | null {
  return draft?.path === path ? draft.adjustments : null;
}

/** Keeps `adjustments` as the drag's draft and renders it; false when no
 * renderer took it (the caller commits normally). */
export function renderDraft(
  path: string,
  adjustments: Adjustments,
  commit: (path: string, adjustments: Adjustments) => void,
): boolean {
  if (!renderer?.(path, adjustments)) return false;
  draft = { path, adjustments };
  lastRendered = adjustments;
  committer = commit;
  commitTimer ??= setTimeout(flushSliderDraft, COMMIT_INTERVAL_MS);
  notify();
  return true;
}

/** Replaces the pending draft of `path` with `update` of it and renders it:
 * data that arrived for the recipe being dragged, not a move. */
export function updateDraft(path: string, update: (adjustments: Adjustments) => Adjustments) {
  if (draft?.path !== path) return;
  const next = update(draft.adjustments);
  if (next === draft.adjustments) return;
  draft = { path, adjustments: next };
  if (renderer?.(path, next)) lastRendered = next;
  notify();
}

/** Commits a pending draft now (drag ended, another edit, photo switch). */
export function flushSliderDraft() {
  if (commitTimer !== null) clearTimeout(commitTimer);
  commitTimer = null;
  const pending = draft;
  draft = null;
  if (pending && committer) committer(pending.path, pending.adjustments);
  if (pending) notify();
}

/** Whether `adjustments` is what the draft renderer showed last. */
export function wasRenderedAsDraft(adjustments: Adjustments) {
  return adjustments === lastRendered;
}
