import { describe, expect, it } from 'vitest';
import {
  dragLinearLine,
  linearFromDrag,
  linearLineOffsets,
  rangesOf,
  rotateLinear,
  signedDistance,
  type LinearParams,
} from './linearMask';

const near = (value: number, expected: number, digits = 6) => expect(value).toBeCloseTo(expected, digits);

// The engine's linear coverage (mask_generation.rs), including rangeEnd.
function coverage(params: LinearParams, point: { x: number; y: number }) {
  const { start, end } = rangesOf(params);
  const d = signedDistance(params, point);
  return Math.min(1, Math.max(0, 0.5 - (d / (d > 0 ? end : start)) * 0.5));
}

// Central line along y = 100 from x 0 → 200: the start (100%) side is y < 100.
const base: LinearParams = { startX: 0, startY: 100, endX: 200, endY: 100, range: 20 };
const lineAt = (params: LinearParams, line: 'start' | 'end') => {
  // Points on the line, measured along the normal from the old central line.
  return linearLineOffsets(params)[line] + signedDistance(base, { x: params.startX, y: params.startY });
};

describe('linear gradient geometry', () => {
  it('matches the engine: 100% on the start side, 0% on the end side', () => {
    expect(signedDistance(base, { x: 50, y: 80 })).toBeLessThan(0);
    near(coverage(base, { x: 50, y: 80 }), 1);
    near(coverage(base, { x: 50, y: 100 }), 0.5);
    near(coverage(base, { x: 50, y: 120 }), 0);
    const asymmetric = { ...base, rangeEnd: 60 };
    near(coverage(asymmetric, { x: 50, y: 130 }), 0.25);
    expect(linearLineOffsets(asymmetric)).toEqual({ start: -20, end: 60 });
  });

  it('draws from the 100% point to the 0% point, like Capture One', () => {
    const from = { x: 300, y: 50 };
    const to = { x: 360, y: 130 };
    const drawn = linearFromDrag(base, from, to, 40);
    near(coverage(drawn, from), 1);
    near(coverage(drawn, to), 0);
    near(coverage(drawn, { x: 330, y: 90 }), 0.5);
    expect(drawn.rangeEnd).toBeUndefined();
    // Shift locks the direction to 45° steps: a nearly vertical drag becomes vertical.
    const snapped = linearFromDrag(base, { x: 0, y: 0 }, { x: 4, y: 100 }, 40, true);
    near(snapped.startY, snapped.endY);
  });
});

describe('dragging the start and end lines', () => {
  it('keeps the other line and moves the central line by default', () => {
    const next = dragLinearLine(base, 'start', { x: 120, y: 40 });
    near(lineAt(next, 'end'), 20);
    near(lineAt(next, 'start'), -60);
    near(coverage(next, { x: 10, y: 40 }), 1);
    near(coverage(next, { x: 10, y: 120 }), 0);
    near(rangesOf(next).start, 40);
    expect(next.rangeEnd).toBeUndefined();
  });

  it('keeps the zone ratio of an asymmetric gradient by default', () => {
    const asymmetric = { ...base, rangeEnd: 60 };
    const next = dragLinearLine(asymmetric, 'end', { x: 0, y: 260 });
    near(lineAt(next, 'start'), -20);
    near(lineAt(next, 'end'), 160);
    near(rangesOf(next).end / rangesOf(next).start, 3);
  });

  it('changes only the dragged zone with Alt', () => {
    const next = dragLinearLine(base, 'end', { x: 70, y: 175 }, 'independent');
    near(next.startY, 100);
    expect(rangesOf(next)).toEqual({ start: 20, end: 75 });
    near(coverage(next, { x: 5, y: 175 }), 0);
    near(coverage(next, { x: 5, y: 80 }), 1);
  });

  it('resizes both zones around the central line with Shift', () => {
    const next = dragLinearLine({ ...base, rangeEnd: 60 }, 'start', { x: 0, y: 65 }, 'symmetric');
    near(next.startY, 100);
    expect(rangesOf(next)).toEqual({ start: 35, end: 35 });
    expect(next.rangeEnd).toBeUndefined();
  });

  it('never lets a line cross the central or the opposite line', () => {
    const crossed = dragLinearLine(base, 'start', { x: 0, y: 150 }, 'independent');
    expect(rangesOf(crossed).start).toBeGreaterThan(0);
    const beyond = dragLinearLine(base, 'start', { x: 0, y: 500 });
    expect(lineAt(beyond, 'start')).toBeLessThan(lineAt(beyond, 'end'));
  });
});

describe('rotating', () => {
  it('turns the central line around the pivot by the pointer angle', () => {
    const pivot = { x: 100, y: 100 };
    const next = rotateLinear(base, pivot, { x: 150, y: 100 }, { x: 100, y: 150 });
    near(next.startX, 100);
    near(next.startY, 0);
    near(next.endX, 100);
    near(next.endY, 200);
    expect(next.range).toBe(20);
  });

  it('locks to 45° steps with Shift', () => {
    const pivot = { x: 100, y: 100 };
    const next = rotateLinear(base, pivot, { x: 150, y: 100 }, { x: 150, y: 140 }, true);
    const angle = (Math.atan2(next.endY - next.startY, next.endX - next.startX) * 180) / Math.PI;
    near(angle, 45, 4);
  });
});


describe('two-sided bands', () => {
  const band = { ...base, rangeEnd: 30, band: { width: 40, feather: 10 } };
  it('places the fully covered edges symmetrically and resizes from either side', () => {
    expect(linearLineOffsets(band)).toEqual({ start: -20, end: 20 });
    for (const [line, y] of [['start', 60], ['end', 140]] as const) {
      const next = dragLinearLine(band, line, { x: 60, y });
      expect(next.band).toEqual({ width: 80, feather: 10 });
      expect(next.startY).toBe(100);
      expect(next.rangeEnd).toBe(30);
    }
  });
  it('edits outward feather independently of width, including hard edges', () => {
    for (const [line, y] of [['featherStart', 50], ['featherEnd', 150]] as const) {
      expect(dragLinearLine(band, line, { x: 80, y }).band).toEqual({ width: 40, feather: 30 });
    }
    expect(dragLinearLine(band, 'featherEnd', { x: 80, y: 110 }).band).toEqual({ width: 40, feather: 0 });
  });
  it('covers the whole dragged span when a band is drawn', () => {
    const from = { x: 300, y: 100 };
    const to = { x: 300, y: 1100 };
    const drawn = linearFromDrag(band, from, to, 40);
    expect(drawn.band).toEqual({ width: 1000, feather: 10 });
    // Press and release lie on the band's fully covered edges.
    expect(signedDistance(drawn, from)).toBeCloseTo(linearLineOffsets(drawn).start);
    expect(signedDistance(drawn, to)).toBeCloseTo(linearLineOffsets(drawn).end);
  });
  it('keeps band dimensions when moved or rotated', () => {
    const rotated = rotateLinear(band, { x: 100, y: 100 }, { x: 200, y: 100 }, { x: 100, y: 200 });
    expect(rotated.band).toEqual(band.band);
    expect(dragLinearLine(rotated, 'end', { x: 60, y: 100 }).band?.width).toBeCloseTo(80);
  });
});
