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
 * region curve under the tone curve, refinement enable states) follow at
 * that rate, the photo and the dragged control itself on every move. At
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
  return true;
}

/** Commits a pending draft now (drag ended, another edit, photo switch). */
export function flushSliderDraft() {
  if (commitTimer !== null) clearTimeout(commitTimer);
  commitTimer = null;
  const pending = draft;
  draft = null;
  if (pending && committer) committer(pending.path, pending.adjustments);
}

/** Whether `adjustments` is what the draft renderer showed last. */
export function wasRenderedAsDraft(adjustments: Adjustments) {
  return adjustments === lastRendered;
}
