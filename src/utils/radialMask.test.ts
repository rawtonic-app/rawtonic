import { describe, expect, it } from 'vitest';
import {
  bellCoverage,
  dragRadialHandle,
  dragRadialLine,
  featherOf,
  radialHandlePositions,
  radialLines,
  toImage,
  toLocal,
  type RadialParams
} from './radialMask';

const base: RadialParams = { centerX: 400, centerY: 300, radiusX: 200, radiusY: 100, rotation: 0, feather: 0.5 };
const near = (value: number, expected: number) => expect(value).toBeCloseTo(expected, 6);

// The engine's radial coverage (mask_generation.rs) at a point in the mask frame.
function engineCoverage(params: RadialParams, point: { x: number; y: number }) {
  const dist = Math.hypot(point.x / params.radiusX, point.y / params.radiusY);
  const innerBound = 1 - featherOf(params);
  return Math.min(1, Math.max(0, 1 - (dist - innerBound) / Math.max(1 - innerBound, 0.01)));
}

describe('radial mask lines', () => {
  it('places the inner, central and outer lines at 100%, 50% and 0% coverage', () => {
    for (const feather of [0.1, 0.5, 0.9]) {
      const params = { ...base, feather };
      const { inner, central, outer } = radialLines(params);
      near(engineCoverage(params, { x: inner.x, y: 0 }), 1);
      near(engineCoverage(params, { x: 0, y: central.y }), 0.5);
      near(engineCoverage(params, { x: outer.x, y: 0 }), 0);
    }
  });

  it('defaults a missing or invalid feather to 50% and clamps it', () => {
    expect(featherOf({ ...base, feather: undefined })).toBe(0.5);
    expect(featherOf({ ...base, feather: Number.NaN })).toBe(0.5);
    expect(featherOf({ ...base, feather: 3 })).toBe(1);
    expect(featherOf({ ...base, feather: -1 })).toBe(0);
  });

  it('converts between image and mask coordinates like the engine, including rotation', () => {
    const rotated = { ...base, rotation: 30 };
    const local = toLocal(rotated, 510, 377);
    const back = toImage(rotated, local);
    near(back.x, 510);
    near(back.y, 377);
    // A point along the rotated x axis has no y component in the mask frame.
    const onAxis = toImage(rotated, { x: 50, y: 0 });
    near(toLocal(rotated, onAxis.x, onAxis.y).y, 0);
    const handles = radialHandlePositions(rotated);
    near(toLocal(rotated, handles.right.x, handles.right.y).x, radialLines(rotated).central.x);
    near(toLocal(rotated, handles.top.x, handles.top.y).y, -radialLines(rotated).central.y);
  });
});

describe('dragging the feather lines', () => {
  it('moves only the 100% area with the inner line', () => {
    const next = dragRadialLine(base, 'inner', { x: 60, y: 0 });
    near(next.radiusX, 200);
    near(next.radiusY, 100);
    near(radialLines(next).inner.x, 60);
    expect(next.centerX).toBe(400);
    // Beyond the outer line the transition vanishes, at the centre it is all feather.
    near(featherOf(dragRadialLine(base, 'inner', { x: 260, y: 0 })), 0);
    near(featherOf(dragRadialLine(base, 'inner', { x: 0, y: 0 })), 1);
  });

  it('keeps the 100% area when the outer line moves', () => {
    const next = dragRadialLine(base, 'outer', { x: 0, y: 150 });
    near(radialLines(next).inner.x, 100);
    near(radialLines(next).inner.y, 50);
    near(radialLines(next).outer.y, 150);
    // It cannot pass inside the 100% area.
    near(featherOf(dragRadialLine(base, 'outer', { x: 40, y: 0 })), 0);
    near(radialLines(dragRadialLine(base, 'outer', { x: 40, y: 0 })).outer.x, 100);
  });

  it('scales a fully feathered mask with the outer line', () => {
    const next = dragRadialLine({ ...base, feather: 1 }, 'outer', { x: 300, y: 0 });
    near(next.radiusX, 300);
    near(next.radiusY, 150);
    expect(next.feather).toBe(1);
  });

  it('keeps the central line with Shift and changes only the overall feathering', () => {
    const central = radialLines(base).central;
    for (const [line, point] of [
      ['inner', { x: 120, y: 0 }],
      ['outer', { x: 0, y: 95 }],
      ['inner', { x: 0, y: 10 }],
      ['outer', { x: 290, y: 0 }]
    ] as const) {
      const next = dragRadialLine(base, line, point, true);
      near(radialLines(next).central.x, central.x);
      near(radialLines(next).central.y, central.y);
    }
    near(radialLines(dragRadialLine(base, 'inner', { x: 120, y: 0 }, true)).inner.x, 120);
    near(radialLines(dragRadialLine(base, 'outer', { x: 0, y: 95 }, true)).outer.y, 95);
    // At most fully feathered (outer at twice the central line), at least hard-edged.
    expect(dragRadialLine(base, 'outer', { x: 1000, y: 0 }, true).feather).toBe(1);
    expect(dragRadialLine(base, 'inner', { x: 1000, y: 0 }, true).feather).toBe(0);
  });

  it('follows a rotated mask through its own frame', () => {
    const rotated = { ...base, rotation: 45 };
    const point = toImage(rotated, { x: 80, y: 0 });
    const next = dragRadialLine(rotated, 'inner', toLocal(rotated, point.x, point.y));
    near(radialLines(next).inner.x, 80);
    expect(next.rotation).toBe(45);
  });
});

describe('dragging the shape handles', () => {
  it('resizes one axis around the centre and keeps the feather', () => {
    const central = radialLines(base).central;
    const next = dragRadialHandle(base, 'right', { x: central.x * 1.5, y: 30 });
    near(next.radiusX, 300);
    near(next.radiusY, 100);
    near(featherOf(next), 0.5);
    expect([next.centerX, next.centerY]).toEqual([400, 300]);
    near(dragRadialHandle(base, 'top', { x: 0, y: -central.y * 2 }).radiusY, 200);
  });

  it('keeps the opposite handle in place with Alt', () => {
    const before = radialHandlePositions({ ...base, rotation: 20 });
    const rotated = { ...base, rotation: 20 };
    const next = dragRadialHandle(
      rotated,
      'bottom',
      { x: 0, y: radialLines(rotated).central.y + 40 },
      { lockOpposite: true }
    );
    const after = radialHandlePositions(next);
    near(after.top.x, before.top.x);
    near(after.top.y, before.top.y);
    near(radialLines(next).central.y, radialLines(rotated).central.y + 20);
    near(next.radiusX, rotated.radiusX);
  });

  it('scales the whole mask with Shift', () => {
    const next = dragRadialHandle(base, 'left', { x: -radialLines(base).central.x / 2, y: 0 }, { uniform: true });
    near(next.radiusX, 100);
    near(next.radiusY, 50);
    expect([next.centerX, next.centerY]).toEqual([400, 300]);
  });

  it('never collapses or inverts the mask', () => {
    const next = dragRadialHandle(base, 'right', { x: -500, y: 0 });
    expect(next.radiusX).toBeGreaterThanOrEqual(1);
    expect(dragRadialLine(base, 'outer', { x: 0, y: 0 }).radiusX).toBeGreaterThanOrEqual(1);
  });
});

describe('version 2 (bell) radial gradients', () => {
  const bell: RadialParams = { ...base, gradientVersion: 2 };
  const ellipticalOf = (params: RadialParams, point: { x: number; y: number }) =>
    Math.hypot(point.x / params.radiusX, point.y / params.radiusY);

  it('draws its lines where 98%, 50% and 2% of the effect are left', () => {
    for (const feather of [0.1, 0.25, 0.5, 0.8, 1]) {
      const params = { ...bell, feather };
      const { inner, central, outer } = radialLines(params);
      near(bellCoverage(ellipticalOf(params, { x: inner.x, y: 0 }), feather), 0.98);
      near(bellCoverage(ellipticalOf(params, { x: 0, y: central.y }), feather), 0.5);
      near(bellCoverage(ellipticalOf(params, { x: outer.x, y: 0 }), feather), 0.02);
      // The effect runs out beyond the ellipse (the radius).
      expect(outer.x).toBeGreaterThan(params.radiusX);
    }
  });

  it('matches the engine at the knots (mask_generation BELL_KNOTS)', () => {
    near(bellCoverage(0.786, 1), 0.5);
    near(bellCoverage(0.885, 0.5), 0.5);
    expect(bellCoverage(1.1, 1)).toBeCloseTo(0.1, 1);
    expect(bellCoverage(0.99, 0)).toBe(1);
    expect(bellCoverage(1.01, 0)).toBe(0);
  });

  it('moves only the full-effect area with the inner line', () => {
    const { outer } = radialLines(bell);
    const next = dragRadialLine(bell, 'inner', { x: 120, y: 0 });
    const lines = radialLines(next);
    near(lines.inner.x, 120);
    near(lines.outer.x, outer.x);
    near(lines.outer.y, outer.y);
    expect(next.gradientVersion).toBe(2);
  });

  it('keeps the full-effect area when the outer line moves', () => {
    const { inner } = radialLines(bell);
    const lines = radialLines(dragRadialLine(bell, 'outer', { x: 260, y: 0 }));
    near(lines.outer.x, 260);
    near(lines.inner.x, inner.x);
  });

  it('scales the whole mask once the outer line goes past the softest feather', () => {
    const soft = { ...bell, feather: 1 };
    const next = dragRadialLine(soft, 'outer', { x: 5000, y: 0 });
    near(radialLines(next).outer.x, 5000);
    near(featherOf(next), 1);
  });

  it('keeps the central line with Shift', () => {
    const { central } = radialLines(bell);
    for (const [line, x] of [
      ['inner', 140],
      ['outer', 230]
    ] as const) {
      const lines = radialLines(dragRadialLine(bell, line, { x, y: 0 }, true));
      near(lines.central.x, central.x);
      near(lines.central.y, central.y);
      near(lines[line].x, x);
    }
  });

  it('leaves version 1 gradients on the straight ramp', () => {
    const { inner, central, outer } = radialLines(base);
    near(inner.x, 100);
    near(central.x, 150);
    near(outer.x, 200);
  });
});
