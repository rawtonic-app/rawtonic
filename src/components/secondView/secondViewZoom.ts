/** Zoom and pan of the second window (pure; the native side draws). All
 * sizes in device px, the photograph's in full-resolution px. */

export interface Zoom {
  scale: number;
  center: [number, number];
}

/** The photograph contained and centred in a `area` (width, height). */
export function fitRect(area: [number, number], shown: [number, number]): [number, number, number, number] {
  const [w, h] = area;
  const [sw, sh] = shown;
  if (sw <= 0 || sh <= 0 || w <= 0 || h <= 0) return [0, 0, 0, 0];
  const scale = Math.min(w / sw, h / sh);
  const fw = sw * scale;
  const fh = sh * scale;
  return [(w - fw) / 2, (h - fh) / 2, fw, fh];
}

/** `center` kept so the photograph covers the window where it is larger
 * than it, and centred where it is smaller (no empty side to pan into). */
export function clampCenter(area: [number, number], shown: [number, number], zoom: Zoom): Zoom {
  const axis = (i: 0 | 1) => {
    const size = shown[i] * zoom.scale;
    if (size <= area[i]) return 0.5;
    const half = area[i] / 2 / size;
    return Math.min(1 - half, Math.max(half, zoom.center[i]));
  };
  return { scale: zoom.scale, center: [axis(0), axis(1)] };
}

/** Fit ↔ 100 % at `point` (device px in the window): zooming in keeps the
 * photograph point under the pointer where it was (Lightroom's click). */
export function toggleAt(
  area: [number, number],
  shown: [number, number],
  zoom: Zoom | null,
  point: [number, number]
): Zoom | null {
  if (zoom) return null;
  const fit = fitRect(area, shown);
  if (fit[2] <= 0 || fit[3] <= 0) return null;
  const u = Math.min(1, Math.max(0, (point[0] - fit[0]) / fit[2]));
  const v = Math.min(1, Math.max(0, (point[1] - fit[1]) / fit[3]));
  const scale = 1;
  const center: [number, number] = [
    u - (point[0] - area[0] / 2) / (shown[0] * scale),
    v - (point[1] - area[1] / 2) / (shown[1] * scale)
  ];
  return clampCenter(area, shown, { scale, center });
}

/** Where a frame of the photograph sits in the window at `zoom` (device
 * px; crates/gpu multi_view.rs `zoomed_rect`): the photograph enlarged by
 * `zoom.scale` with `zoom.center` at the window's centre, the frame
 * covering its `region` of it (null: the whole photograph). */
export function zoomedRect(
  area: [number, number],
  shown: [number, number],
  region: [number, number, number, number] | null,
  zoom: Zoom
): [number, number, number, number] {
  const s = zoom.scale;
  const ox = area[0] / 2 - zoom.center[0] * shown[0] * s;
  const oy = area[1] / 2 - zoom.center[1] * shown[1] * s;
  if (!region) return [ox, oy, shown[0] * s, shown[1] * s];
  return [ox + region[0] * s, oy + region[1] * s, region[2] * s, region[3] * s];
}

/** The zoom after the pointer moved by `delta` (device px) while dragging. */
export function panBy(area: [number, number], shown: [number, number], zoom: Zoom, delta: [number, number]): Zoom {
  return clampCenter(area, shown, {
    scale: zoom.scale,
    center: [zoom.center[0] - delta[0] / (shown[0] * zoom.scale), zoom.center[1] - delta[1] / (shown[1] * zoom.scale)]
  });
}
