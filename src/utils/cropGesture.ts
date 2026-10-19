/** Crop resize and move gestures in the crop tool: their frame lives in the
 * canvas until release (then it is committed as the recipe's crop). An
 * asynchronous crop answer (Auto straighten) asked for before or during one
 * is out of date: it compares `cropGestures()` at its request and at its
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

/** How many gestures have started, and whether one is in progress. */
export function cropGestures(): { started: number; active: boolean } {
  return { started, active: active > 0 };
}
