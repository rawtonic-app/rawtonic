/**
 * Whether a pointer is held down anywhere in the window (a brush stroke or
 * a drag under way), and running something only once it is released. The
 * full screen changes the viewer's size, so it opens after a stroke ends,
 * never in the middle of one (a stroke keeps its points in screen
 * coordinates until its release).
 */
let held = false;

if (typeof window !== 'undefined') {
  const track = (e: PointerEvent) => {
    held = e.type === 'pointerdown';
  };
  window.addEventListener('pointerdown', track, true);
  window.addEventListener('pointerup', track, true);
  window.addEventListener('pointercancel', track, true);
  window.addEventListener('blur', () => {
    held = false;
  });
}

export function pointerHeld(): boolean {
  return held;
}

/** Runs `action` now, or after the held pointer's release once the
 * gesture's own release handlers have run (the release is seen in the
 * capture phase, so a handler that stops it does not keep it from here).
 * Losing the window's focus before the release drops it: the gesture has
 * not finished. Returns a cancel for an action still waiting. */
export function afterPointerRelease(action: () => void): () => void {
  if (!held) {
    action();
    return () => {};
  }
  let timer: ReturnType<typeof setTimeout> | undefined;
  const stop = () => {
    window.removeEventListener('pointerup', released, true);
    window.removeEventListener('pointercancel', released, true);
    window.removeEventListener('blur', stop);
  };
  const released = () => {
    stop();
    timer = setTimeout(action, 0);
  };
  window.addEventListener('pointerup', released, true);
  window.addEventListener('pointercancel', released, true);
  window.addEventListener('blur', stop);
  return () => {
    stop();
    clearTimeout(timer);
  };
}
