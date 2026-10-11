import { expect, it } from 'vitest';
import { brushGestureValues, isBrushGesturePress } from './brushGesture';

const start = { size: 500, hardness: 50, opacity: 100, flow: 100 };

it('sizes sideways in image pixels with the ring edge under the pointer, and hardens upwards', () => {
  // At 0.25 screen pixels per image pixel, 10 px sideways widen the radius by
  // 40 image pixels, the diameter by 80.
  expect(brushGestureValues(start, 10, 0, false, 0.25)).toEqual({ ...start, size: 580 });
  expect(brushGestureValues(start, 0, -20, false)).toEqual({ ...start, hardness: 55 });
  expect(brushGestureValues(start, -10000, 1000, false)).toEqual({ ...start, size: 1, hardness: 0 });
  expect(brushGestureValues(start, 100000, 0, false)).toEqual({ ...start, size: 6000 });
});

it('sets opacity sideways and flow up and down with Shift, a quarter percent per pixel', () => {
  expect(brushGestureValues(start, -40, 80, true)).toEqual({ ...start, opacity: 90, flow: 80 });
  expect(brushGestureValues(start, -1000, 1000, true)).toEqual({ ...start, opacity: 1, flow: 1 });
});

it('starts only for a primary press with Control and Option', () => {
  expect(isBrushGesturePress({ button: 0, ctrlKey: true, altKey: true })).toBe(true);
  expect(isBrushGesturePress({ button: 0, ctrlKey: true, altKey: false })).toBe(false);
  expect(isBrushGesturePress({ button: 2, ctrlKey: true, altKey: true })).toBe(false);
});
