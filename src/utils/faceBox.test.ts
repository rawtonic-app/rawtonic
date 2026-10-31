import { expect, it } from 'vitest';
import { labelPlace, outlineOnPhoto, svgPoints } from './faceBox';
import { fitInViewer } from './viewerDisplay';

// A 6000 × 4000 photograph (mask frame) in a 1200 × 900 CSS-pixel viewer.
const square: [number, number][] = [
  [3000, 1000],
  [3400, 1000],
  [3400, 1400],
  [3000, 1400],
];

/** Where the native view shows mask-frame pixel `p` on screen (CSS pixels):
 * the crop's frame fitted into the viewer, zoomed and panned about the
 * viewer's top left as the transform wrapper does. Independent of the
 * overlay's own mapping. */
function nativeScreen(
  p: [number, number],
  crop: { x: number; y: number; width: number; height: number },
  zoom: number,
  pan: { x: number; y: number },
) {
  const fit = fitInViewer(1200, 900, crop.width, crop.height);
  return {
    x: pan.x + zoom * (fit.offsetX + ((p[0] - crop.x) * fit.width) / crop.width),
    y: pan.y + zoom * (fit.offsetY + ((p[1] - crop.y) * fit.height) / crop.height),
  };
}

/** The overlay's point on screen: its box placed at the fit's offset inside
 * the zoomed content. */
function overlayScreen(
  q: { x: number; y: number },
  crop: { x: number; y: number; width: number; height: number },
  zoom: number,
  pan: { x: number; y: number },
) {
  const fit = fitInViewer(1200, 900, crop.width, crop.height);
  return { x: pan.x + zoom * (fit.offsetX + q.x), y: pan.y + zoom * (fit.offsetY + q.y) };
}

it('lands on the native photograph uncropped, cropped and at any zoom and pan', () => {
  for (const crop of [
    { x: 0, y: 0, width: 6000, height: 4000 },
    { x: 2500, y: 600, width: 1500, height: 1500 },
    { x: 1200, y: 300, width: 4000, height: 2000 },
  ]) {
    const fit = fitInViewer(1200, 900, crop.width, crop.height);
    const points = outlineOnPhoto(square, crop.x, crop.y, fit.scale);
    for (const [zoom, pan] of [
      [1, { x: 0, y: 0 }],
      [2.5, { x: -800, y: -300 }],
      [8, { x: -9000, y: -4000 }],
    ] as const) {
      points.forEach((q, i) => {
        const a = overlayScreen(q, crop, zoom, pan);
        const b = nativeScreen(square[i], crop, zoom, pan);
        expect(Math.abs(a.x - b.x)).toBeLessThan(1e-6);
        expect(Math.abs(a.y - b.y)).toBeLessThan(1e-6);
      });
    }
  }
});

it('keeps a turned outline turned (the backend carries the rotation)', () => {
  // A box straightened by 30° as the backend returns it.
  const c = { x: 3200, y: 1200 };
  const turned = square.map(([x, y]) => {
    const r = (30 * Math.PI) / 180;
    const dx = x - c.x;
    const dy = y - c.y;
    return [c.x + dx * Math.cos(r) - dy * Math.sin(r), c.y + dx * Math.sin(r) + dy * Math.cos(r)] as [number, number];
  });
  const points = outlineOnPhoto(turned, 1000, 500, 0.25);
  const angle = (Math.atan2(points[1].y - points[0].y, points[1].x - points[0].x) * 180) / Math.PI;
  expect(angle).toBeCloseTo(30, 6);
  expect(svgPoints(points).split(' ')).toHaveLength(4);
});

it('is in CSS pixels, the same on every display density', () => {
  // The fit is in CSS pixels; the native view renders it at the display's
  // density, so the overlay is the same at 1×, 2× and 3×, and a device
  // pixel of the box is the native frame's device pixel.
  const fit = fitInViewer(1200, 900, 6000, 4000);
  const at1 = outlineOnPhoto(square, 0, 0, fit.scale);
  for (const dpr of [1, 2, 3]) {
    const native = square.map(([x, y]) => ({ x: x * fit.scale * dpr, y: y * fit.scale * dpr }));
    at1.forEach((q, i) => {
      expect(q.x * dpr).toBeCloseTo(native[i].x, 9);
      expect(q.y * dpr).toBeCloseTo(native[i].y, 9);
    });
  }
});

it('puts the label above the box, below its top edge at the photograph’s top', () => {
  const points = outlineOnPhoto(square, 0, 0, 0.2);
  expect(labelPlace(points, 10)).toEqual({ x: 600, y: 200, below: false });
  const top = outlineOnPhoto(square, 0, 995, 0.2);
  expect(labelPlace(top, 10).below).toBe(true);
});
