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
 * gesture's own release handlers have run. */
export function afterPointerRelease(action: () => void): void {
  if (!held) {
    action();
    return;
  }
  const released = () => {
    window.removeEventListener('pointerup', released);
    window.removeEventListener('pointercancel', released);
    window.removeEventListener('blur', released);
    setTimeout(action, 0);
  };
  window.addEventListener('pointerup', released);
  window.addEventListener('pointercancel', released);
  window.addEventListener('blur', released);
}
