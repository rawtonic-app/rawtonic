// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * What the Library shows when an import ends. Lightroom shows the new
 * photographs after Import; but a photographer who started culling while the
 * import ran must not be moved away from the photograph they are rating: at
 * the end the view changed to Previous Import, the selection emptied, and
 * every later rating key rated nothing (RATE2, Lenovo WRATE2 set D).
 */

/** Whether the user pressed a key, clicked, touched or scrolled since `watchUserActivity`. */
export interface UserActivity {
  acted(): boolean;
  stop(): void;
}

const EVENTS = ['keydown', 'pointerdown', 'wheel'] as const;

/** Watches `target` (the window) for the user's own input, from now on. */
export function watchUserActivity(
  target: Pick<Window, 'addEventListener' | 'removeEventListener'> = window
): UserActivity {
  let acted = false;
  const note = () => {
    acted = true;
  };
  for (const type of EVENTS) target.addEventListener(type, note, { capture: true, passive: true });
  return {
    acted: () => acted,
    stop: () => {
      for (const type of EVENTS) target.removeEventListener(type, note, { capture: true });
    }
  };
}

/**
 * `show`: switch to Previous Import (nobody touched the app while it ran);
 * `offer`: keep what is shown and offer Show in the import's notice;
 * `stay`: nothing new to show.
 */
export type ImportEndView = 'show' | 'offer' | 'stay';

export function importEndView({
  imported,
  viewerShown,
  sameView,
  acted
}: {
  /** At least one photograph was added. */
  imported: boolean;
  /** Develop, the loupe or full screen shows a photograph. */
  viewerShown: boolean;
  /** The Library still shows the view the import showed. */
  sameView: boolean;
  /** The user pressed a key, clicked or scrolled since the import began. */
  acted: boolean;
}): ImportEndView {
  if (!imported) return 'stay';
  if (viewerShown || !sameView || acted) return 'offer';
  return 'show';
}
