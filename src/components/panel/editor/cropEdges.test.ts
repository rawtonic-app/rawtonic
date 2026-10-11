import { expect, it } from 'vitest';
import { isCropEdge, resizeCropFromEdge } from './cropEdges';

const start = { x: 100, y: 100, width: 300, height: 200 };
const aspect = 1.5;

it('moves the dragged side and keeps the ratio centred on the other axis', () => {
  // Top edge up by 20: 20 taller, 30 wider, centred horizontally.
  expect(resizeCropFromEdge(start, 'n', 0, -20, aspect, 1000, 1000)).toEqual({ x: 85, y: 80, width: 330, height: 220 });
  // Right edge in by 30: bottom/top centred.
  expect(resizeCropFromEdge(start, 'e', -30, 0, aspect, 1000, 1000)).toEqual({ x: 100, y: 110, width: 270, height: 180 });
  expect(resizeCropFromEdge(start, 's', 0, 10, aspect, 1000, 1000)).toEqual({ x: 92.5, y: 100, width: 315, height: 210 });
  expect(resizeCropFromEdge(start, 'w', 60, 0, aspect, 1000, 1000)).toEqual({ x: 160, y: 120, width: 240, height: 160 });
});

it('stops at the photo edge on both axes and at a minimum size', () => {
  // Growing the top edge stops at the photo's top (here before the sides).
  const tall = resizeCropFromEdge(start, 'n', 0, -1000, aspect, 1000, 1000);
  expect(tall).toEqual({ x: 25, y: 0, width: 450, height: 300 });
  // In a narrow photo the room left and right of the centre stops it first.
  // (centre 250, only 150 to the right edge: the frame cannot grow).
  expect(resizeCropFromEdge(start, 'n', 0, -1000, aspect, 400, 1000)).toEqual(start);
  expect(resizeCropFromEdge(start, 'e', -1000, 0, aspect, 1000, 1000).width).toBe(10);
});

it('recognises only side handles', () => {
  expect(['n', 'e', 's', 'w'].every(isCropEdge)).toBe(true);
  expect(isCropEdge('ne')).toBe(false);
  expect(isCropEdge(null)).toBe(false);
});
