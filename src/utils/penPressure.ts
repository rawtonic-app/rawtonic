import { useSyncExternalStore } from 'react';
import type { Coord } from './adjustments';

/** A stroke point; `p` is pen pressure 0–1 (absent: full pressure). */
export interface StrokePoint extends Coord {
  p?: number;
}

/** What pen pressure scales in new strokes (brush settings; absent: size on,
 * flow off, as Capture One's Pen Pressure). */
export interface PressureMapping {
  size: boolean;
  flow: boolean;
}

export function pressureMapping(settings: { pressureSize?: boolean; pressureFlow?: boolean } | null | undefined) {
  return { size: settings?.pressureSize ?? true, flow: !!settings?.pressureFlow };
}

/** The pressure a point stores: a pen's, to three decimals, while a mapping
 * is on; none for a mouse or touch, which paint at full pressure (user
 * decision D3: no simulated pressure). */
export function pointPressure(
  event: Pick<PointerEvent, 'pointerType' | 'pressure'> | null | undefined,
  mapping: PressureMapping,
): number | undefined {
  if (!event || event.pointerType !== 'pen' || !(mapping.size || mapping.flow)) return undefined;
  const pressure = Number.isFinite(event.pressure) ? Math.min(1, Math.max(0, event.pressure)) : 1;
  return Math.round(pressure * 1000) / 1000;
}

export function withPressure<T extends Coord>(point: T, p: number | undefined): T & StrokePoint {
  return p === undefined ? point : { ...point, p };
}

/** The line keys of a stroke whose points carry pressure (none otherwise, so
 * mouse strokes keep their recipe byte for byte). */
export function pressureKeys(points: StrokePoint[], mapping: PressureMapping) {
  if (!points.some((point) => point.p !== undefined)) return {};
  return {
    ...(mapping.size ? { pressureSize: true as const } : {}),
    ...(mapping.flow ? { pressureFlow: true as const } : {}),
  };
}

/** A point closer than the stroke's spacing threshold is still kept when the
 * pen's pressure changed by this much (pressing harder in place). */
export const PRESSURE_STEP = 0.02;

export function pressureChanged(from: StrokePoint, to: StrokePoint) {
  return from.p !== undefined && to.p !== undefined && Math.abs(to.p - from.p) >= PRESSURE_STEP;
}

// Whether the webview has reported a pen: WKWebView and WebKitGTK report
// tablet pens as a mouse (pressure 0.5, WebKit's PointerEvent.cpp), so there
// the pressure settings would do nothing and stay hidden.
let penSeen = false;
const penListeners = new Set<() => void>();
function notePointer(event: PointerEvent) {
  if (penSeen || event.pointerType !== 'pen') return;
  penSeen = true;
  for (const listener of penListeners) listener();
}

function subscribePen(listener: () => void) {
  if (penListeners.size === 0 && !penSeen) {
    window.addEventListener('pointerdown', notePointer, true);
    window.addEventListener('pointermove', notePointer, true);
  }
  penListeners.add(listener);
  return () => {
    penListeners.delete(listener);
    if (penListeners.size === 0) {
      window.removeEventListener('pointerdown', notePointer, true);
      window.removeEventListener('pointermove', notePointer, true);
    }
  };
}

/** True once a pen has been used in this window (it stays true). */
export function usePenAvailable() {
  return useSyncExternalStore(
    subscribePen,
    () => penSeen,
    () => false,
  );
}
