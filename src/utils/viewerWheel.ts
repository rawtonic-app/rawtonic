/**
 * What a wheel event over the photograph means: zoom about the pointer or
 * pan. Decided per event from the device that sent it, as Capture One and
 * Lightroom Classic do on macOS: a trackpad or Magic Mouse scroll pans (both
 * axes, its momentum too), a pinch zooms, a mouse wheel zooms.
 *
 * WKWebView's wheel events carry no device signal: a mouse wheel's line
 * steps and a trackpad's pixel deltas both arrive with `deltaMode` 0 and
 * `wheelDelta = -3 × delta` (measured 2026-11-16 with CoreGraphics events in
 * a WKWebView, notes: reference/ux-macos-review-2026-11-16 UXR4M-01). So the
 * desktop app reads AppKit's own answer (`hasPreciseScrollingDeltas`) and
 * hands it to the page whenever it changes (`window.__rawtonicScrollPrecise`,
 * src-tauri/src/window_customizer.rs `precise_scroll`). Where no such report
 * exists (Windows, Linux, the browser dev server) the Controls preference
 * "Input Device Optimization" decides, and a wheel with ctrlKey — what
 * Chromium and WebKitGTK send for a touchpad pinch — zooms.
 */

declare global {
  interface Window {
    /** Whether the last scroll event had precise (trackpad) deltas; set by
     * the macOS app, absent elsewhere. */
    __rawtonicScrollPrecise?: boolean;
  }
}

export type CanvasInputMode = 'mouse' | 'trackpad' | undefined;

export type WheelAction =
  | {
      kind: 'zoom';
      /** Scale multiplier for this event (> 1 zooms in). */
      factor: number;
    }
  | { kind: 'pan'; dx: number; dy: number };

/** The platform's report for the device behind the last scroll, if any. */
export function preciseScrollReport(): boolean | undefined {
  if (typeof window === 'undefined') return undefined;
  const value = window.__rawtonicScrollPrecise;
  return typeof value === 'boolean' ? value : undefined;
}

type WheelLike = Pick<WheelEvent, 'deltaX' | 'deltaY' | 'ctrlKey' | 'shiftKey' | 'altKey'>;

const ZOOM_SENSITIVITY = 0.002;

/**
 * The action for `e`. `zoomSpeed` is the Controls zoom speed multiplier.
 * Zoom never comes from `deltaX` (a sideways swipe or tilt wheel pans).
 */
export function wheelAction(
  e: WheelLike,
  inputMode: CanvasInputMode,
  zoomSpeed = 1,
  precise: boolean | undefined = preciseScrollReport()
): WheelAction {
  // A device with precise deltas: the platform said so, or the user chose
  // the touchpad where the platform cannot tell.
  const trackpad = precise ?? inputMode === 'trackpad';
  // Pinch deltas are small: the touchpad scale keeps a pinch usable.
  const speed = zoomSpeed * (trackpad ? 5 : 1);
  const zoom = (delta: number): WheelAction => ({
    kind: 'zoom',
    factor: Math.exp(-delta * ZOOM_SENSITIVITY * speed)
  });
  if (e.ctrlKey) return zoom(e.deltaY);
  if (trackpad) return { kind: 'pan', dx: e.deltaX, dy: e.deltaY };
  // A mouse wheel: Shift pans sideways, Option/Alt up and down, both
  // diagonally; a plain wheel zooms. macOS turns Shift + wheel into deltaX.
  const delta = e.deltaY !== 0 ? e.deltaY : e.deltaX;
  if (e.shiftKey && e.altKey) return { kind: 'pan', dx: delta, dy: delta };
  if (e.shiftKey) return { kind: 'pan', dx: delta, dy: 0 };
  if (e.altKey) return { kind: 'pan', dx: 0, dy: delta };
  if (e.deltaY === 0) return { kind: 'pan', dx: e.deltaX, dy: 0 };
  return zoom(e.deltaY);
}

/** WebKit's macOS pinch: `gesturestart` / `gesturechange` with the scale
 * since the gesture began. No wheel event is sent for it. */
export interface PinchGestureEvent extends UIEvent {
  scale: number;
  clientX?: number;
  clientY?: number;
}

/**
 * Follows a WebKit pinch on `element`: `onZoom(factor, clientX, clientY)`
 * for each change, the factor relative to the previous change. Returns the
 * cleanup. `lastPointer` supplies the anchor when the event has none.
 */
export function listenForPinch(
  element: HTMLElement,
  onZoom: (factor: number, clientX: number, clientY: number) => void,
  lastPointer: () => { x: number; y: number } | null
): () => void {
  let previous = 1;
  const anchor = (e: PinchGestureEvent) => {
    if (typeof e.clientX === 'number' && typeof e.clientY === 'number') return { x: e.clientX, y: e.clientY };
    const pointer = lastPointer();
    if (pointer) return pointer;
    const rect = element.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  };
  const start = (event: Event) => {
    event.preventDefault();
    previous = 1;
  };
  const change = (event: Event) => {
    event.preventDefault();
    const e = event as PinchGestureEvent;
    if (!(e.scale > 0) || !Number.isFinite(e.scale)) return;
    const factor = e.scale / previous;
    previous = e.scale;
    if (factor === 1) return;
    const { x, y } = anchor(e);
    onZoom(factor, x, y);
  };
  const end = (event: Event) => event.preventDefault();
  element.addEventListener('gesturestart', start, { passive: false });
  element.addEventListener('gesturechange', change, { passive: false });
  element.addEventListener('gestureend', end, { passive: false });
  return () => {
    element.removeEventListener('gesturestart', start);
    element.removeEventListener('gesturechange', change);
    element.removeEventListener('gestureend', end);
  };
}
