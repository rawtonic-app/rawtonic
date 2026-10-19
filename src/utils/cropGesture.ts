import { useCallback, useEffect, useRef } from 'react';

/** Crop gestures in the crop tool: resize and move (their frame lives in
 * the canvas until release, then it is committed as the recipe's crop) and
 * cancelling the crop edit (Escape restores the crop the tool opened with).
 * An asynchronous crop answer (Auto straighten) asked for before or during
 * one is out of date: it compares `cropGestures()` at its request and at its
 * answer. */
let started = 0;
let active = 0;

export function cropGestureStarted() {
  started += 1;
  active += 1;
}

export function cropGestureEnded() {
  active = Math.max(0, active - 1);
}

/** Cancelling the crop edit: a gesture that starts and ends at once. */
export function cropEditCancelled() {
  started += 1;
}

/** How many gestures have started, and whether one is in progress. */
export function cropGestures(): { started: number; active: boolean } {
  return { started, active: active > 0 };
}

/** A component's crop drags: `start`/`end` count them, and a component that
 * unmounts during one (the photo or view left mid-drag) ends it. */
export function useCropGestures(): { start: () => void; end: () => void } {
  const open = useRef(0);
  const start = useCallback(() => {
    open.current += 1;
    cropGestureStarted();
  }, []);
  const end = useCallback(() => {
    if (open.current === 0) return;
    open.current -= 1;
    cropGestureEnded();
  }, []);
  useEffect(
    () => () => {
      while (open.current > 0) {
        open.current -= 1;
        cropGestureEnded();
      }
    },
    [],
  );
  return { start, end };
}
