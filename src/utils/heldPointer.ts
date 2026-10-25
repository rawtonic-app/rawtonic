/**
 * Whether a pointer is held down anywhere in the window (a brush stroke or
 * a drag under way). The full screen changes the viewer's size, and a
 * stroke keeps its points in screen coordinates until its release, so F
 * does nothing in the middle of one. Fail-safe: a release this misses only
 * keeps F ignored until that pointer's next release, a lost window focus or
 * a hidden window, which all clear it.
 */
const held = new Set<number>();

if (typeof window !== 'undefined') {
  window.addEventListener('pointerdown', (e) => held.add(e.pointerId), true);
  const release = (e: PointerEvent) => held.delete(e.pointerId);
  window.addEventListener('pointerup', release, true);
  window.addEventListener('pointercancel', release, true);
  const clear = () => held.clear();
  window.addEventListener('blur', clear);
  document.addEventListener('visibilitychange', clear);
}

export function pointerHeld(): boolean {
  return held.size > 0;
}
