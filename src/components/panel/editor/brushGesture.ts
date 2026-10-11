import { useEffect, useRef, useState } from 'react';
import type { BrushSettings } from '../../ui/AppProperties';
import { BRUSH_SIZE_MAX, BRUSH_SIZE_MIN, DEFAULT_BRUSH_SIZE } from '../../../utils/brushSize';

/** Brush settings a modifier drag changes, after Capture One: Control+Option
 * dragging sideways sets the size and up/down the hardness; with Shift as well,
 * sideways sets the opacity and up/down the flow. */
export interface BrushGestureValues {
  size: number;
  hardness: number;
  opacity: number;
  flow: number;
}

const clamp = (value: number, min: number, max: number) => Math.round(Math.min(max, Math.max(min, value)));

/** Percent per screen pixel for hardness, opacity and flow: 400 px span the
 * whole range, fine enough to set single steps. */
const PERCENT_PER_PIXEL = 0.25;

/** Values after dragging `dx`, `dy` screen pixels. `screenPixelsPerImagePixel`
 * converts the drag to image pixels, so the size ring's edge follows the
 * pointer sideways (the size is a diameter, hence the factor 2). */
export function brushGestureValues(
  start: BrushGestureValues,
  dx: number,
  dy: number,
  shift: boolean,
  screenPixelsPerImagePixel = 1,
): BrushGestureValues {
  if (shift) {
    return {
      ...start,
      opacity: clamp(start.opacity + dx * PERCENT_PER_PIXEL, 1, 100),
      flow: clamp(start.flow - dy * PERCENT_PER_PIXEL, 1, 100),
    };
  }
  const imagePixels = (2 * dx) / Math.max(1e-6, screenPixelsPerImagePixel);
  return {
    ...start,
    size: clamp(start.size + imagePixels, BRUSH_SIZE_MIN, BRUSH_SIZE_MAX),
    hardness: clamp(start.hardness - dy * PERCENT_PER_PIXEL, 0, 100),
  };
}

/** Whether a press starts a brush gesture rather than a stroke. */
export function isBrushGesturePress(event: Pick<MouseEvent, 'button' | 'ctrlKey' | 'altKey'>) {
  return event.button === 0 && event.ctrlKey && event.altKey;
}

export interface BrushGestureState {
  /** Pointer position relative to the canvas container where the drag began. */
  x: number;
  y: number;
  values: BrushGestureValues;
}

/** Handles brush gestures on `container` while the brush is active. Presses
 * with Control+Option are taken before the canvas sees them (no stroke, and
 * the Control-click context menu stays closed). */
export function useBrushGesture(
  container: React.RefObject<HTMLElement | null>,
  active: boolean,
  settings: BrushSettings | null | undefined,
  screenPixelsPerImagePixel: number,
  apply: (values: BrushGestureValues) => void,
) {
  const [gesture, setGesture] = useState<BrushGestureState | null>(null);
  const latest = useRef({ settings, apply, screenPixelsPerImagePixel });
  latest.current = { settings, apply, screenPixelsPerImagePixel };

  useEffect(() => {
    const element = container.current;
    if (!active || !element) return;
    let ending: (() => void) | null = null;
    const onPointerDown = (event: PointerEvent) => {
      if (!isBrushGesturePress(event)) return;
      event.preventDefault();
      event.stopPropagation();
      const current = latest.current.settings;
      const start: BrushGestureValues = {
        size: current?.size ?? DEFAULT_BRUSH_SIZE,
        hardness: current?.hardness ?? 50,
        opacity: current?.opacity ?? 100,
        flow: current?.flow ?? 100,
      };
      const box = element.getBoundingClientRect();
      const origin = { x: event.clientX, y: event.clientY };
      setGesture({ x: event.clientX - box.left, y: event.clientY - box.top, values: start });
      // Moves are applied once per frame: the canvas re-renders with each
      // change, and a 120 Hz mouse would otherwise queue renders behind it.
      let frame = 0;
      let pending: PointerEvent | null = null;
      const update = () => {
        frame = 0;
        if (!pending) return;
        const values = brushGestureValues(
          start,
          pending.clientX - origin.x,
          pending.clientY - origin.y,
          pending.shiftKey,
          latest.current.screenPixelsPerImagePixel,
        );
        pending = null;
        latest.current.apply(values);
        setGesture((state) => (state ? { ...state, values } : state));
      };
      const onMove = (move: PointerEvent) => {
        move.preventDefault();
        pending = move;
        if (!frame) frame = requestAnimationFrame(update);
      };
      const swallow = (other: Event) => {
        other.preventDefault();
        other.stopPropagation();
      };
      const end = () => {
        if (frame) cancelAnimationFrame(frame);
        update();
        window.removeEventListener('pointermove', onMove, true);
        window.removeEventListener('pointerup', end, true);
        window.removeEventListener('pointercancel', end, true);
        // The mouse events that follow the press must not reach the canvas.
        window.setTimeout(() => {
          for (const type of ['mousedown', 'mouseup', 'click', 'contextmenu'] as const) {
            window.removeEventListener(type, swallow, true);
          }
        }, 0);
        ending = null;
        setGesture(null);
      };
      for (const type of ['mousedown', 'mouseup', 'click', 'contextmenu'] as const) {
        window.addEventListener(type, swallow, true);
      }
      window.addEventListener('pointermove', onMove, true);
      window.addEventListener('pointerup', end, true);
      window.addEventListener('pointercancel', end, true);
      ending = end;
    };
    element.addEventListener('pointerdown', onPointerDown, true);
    return () => {
      element.removeEventListener('pointerdown', onPointerDown, true);
      ending?.();
    };
  }, [container, active]);

  return gesture;
}
