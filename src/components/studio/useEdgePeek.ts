import { useCallback, useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";

/** How long the pointer has to stay on a window-edge strip (12 px) before
 * its hidden side panel comes in: about one frame, so that a pointer
 * crossing the strip on its way out of the window (one event, a few
 * milliseconds) does not flash the panel, while one that reaches the edge,
 * slowly or in one jump, gets it at once. Lightroom Classic 15.6 (Auto Hide
 * & Show, 2026-10-10, pointer events posted like cliclick) shows its panel
 * 35-70 ms after the pointer comes within about 9 pt of the window edge,
 * with no rest; it also flashes the panel for visits of 8 ms. */
export const PEEK_HOLD_MS = 20;
/** The panel goes this long after the pointer has left it, so that a slip
 * over its border does not close it (Lightroom: 130-180 ms). */
export const PEEK_LEAVE_MS = 150;

export type PeekSide = "left" | "right";

const insidePanel = (target: EventTarget | null, side: PeekSide) =>
  target instanceof Element && Boolean(target.closest(`[data-peek="${side}"]`));
const insideEdge = (target: EventTarget | null, side: PeekSide) =>
  target instanceof Element && Boolean(target.closest(`.studio-edge.is-${side}`));

/** A hidden side panel shown for a moment over the photograph (Lightroom's
 * Auto Hide & Show): it comes in as soon as the pointer reaches its edge
 * strip and stays there for a frame, and goes when the pointer leaves the
 * panel. Never while a button is held (a slider, brush or crop drag that
 * reaches the edge); a drag that started in the panel keeps it until
 * release. `resetKey` (module, stored layout) ends
 * the peek when the layout changes. */
export function useEdgePeek(resetKey: unknown) {
  const [peek, setPeek] = useState<PeekSide | null>(null);
  const showTimer = useRef<number | undefined>(undefined);
  const hideTimer = useRef<number | undefined>(undefined);
  const release = useRef<(() => void) | null>(null);

  const clear = (timer: { current: number | undefined }) => {
    if (timer.current !== undefined) window.clearTimeout(timer.current);
    timer.current = undefined;
  };
  const stopWaitingForRelease = () => {
    release.current?.();
    release.current = null;
  };
  const end = useCallback(() => {
    clear(showTimer);
    clear(hideTimer);
    stopWaitingForRelease();
    setPeek(null);
  }, []);
  const endSoon = () => {
    clear(hideTimer);
    hideTimer.current = window.setTimeout(end, PEEK_LEAVE_MS);
  };

  useEffect(() => end, [end]);
  useEffect(() => end(), [resetKey, end]);

  /** Starts the short wait before `side` comes in as the pointer enters or
   * moves on its strip (a button held: none). */
  const approach = (side: PeekSide, e: ReactPointerEvent) => {
    if (peek === side) {
      clear(hideTimer);
      return;
    }
    if (e.buttons !== 0) {
      clear(showTimer);
      return;
    }
    if (showTimer.current !== undefined) return;
    showTimer.current = window.setTimeout(() => {
      showTimer.current = undefined;
      setPeek(side);
    }, PEEK_HOLD_MS);
  };

  const edgeProps = (side: PeekSide) => ({
    onPointerEnter: (e: ReactPointerEvent) => approach(side, e),
    onPointerMove: (e: ReactPointerEvent) => approach(side, e),
    onPointerLeave: (e: ReactPointerEvent) => {
      clear(showTimer);
      if (peek === side && !insidePanel(e.relatedTarget, side)) endSoon();
    },
  });

  const panelProps = (side: PeekSide) =>
    peek === side
      ? {
          "data-peek": side,
          onPointerEnter: () => clear(hideTimer),
          onPointerLeave: (e: ReactPointerEvent) => {
            if (insideEdge(e.relatedTarget, side) || insidePanel(e.relatedTarget, side)) return;
            if (e.buttons === 0) {
              endSoon();
              return;
            }
            // A drag from inside the panel (slider, width): decide on release.
            stopWaitingForRelease();
            const onUp = (up: PointerEvent) => {
              stopWaitingForRelease();
              const over = document.elementFromPoint(up.clientX, up.clientY);
              if (!insidePanel(over, side) && !insideEdge(over, side)) endSoon();
            };
            window.addEventListener("pointerup", onUp, true);
            release.current = () => window.removeEventListener("pointerup", onUp, true);
          },
        }
      : {};

  return { peek, edgeProps, panelProps, endPeek: end };
}
