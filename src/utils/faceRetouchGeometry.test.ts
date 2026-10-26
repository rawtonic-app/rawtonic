import { describe, expect, it } from 'vitest';
import { INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { faceRetouchIsCurrent, frameGeometry } from './faceRetouchGeometry';

describe('the face retouch panel geometry', () => {
  it('takes a geometry with the same keys and values in another order as current (a frame update writes the engine’s order)', () => {
    const adjustments = {
      ...INITIAL_ADJUSTMENTS,
      rotation: 2,
      guidedPerspective: { enabled: true, lines: [{ id: 'a', type: 'vertical', p1: { x: 0.4, y: 0.1 }, p2: { x: 0.4, y: 0.9 } }], autoCrop: false },
    } as Adjustments;
    const panel = JSON.parse(frameGeometry(adjustments, '2:x'));
    const reorder = (v: unknown): unknown =>
      Array.isArray(v) ? v.map(reorder) : v && typeof v === 'object'
        ? Object.fromEntries(Object.entries(v as object).reverse().map(([k, x]) => [k, reorder(x)])) : v;
    const engine = JSON.stringify(reorder(panel));
    expect(engine).not.toBe(frameGeometry(adjustments, '2:x'));
    const retouch = { settings: {}, faces: [], frame: '2:x', geometry: engine };
    expect(faceRetouchIsCurrent(retouch, adjustments)).toBe(true);
    expect(faceRetouchIsCurrent(retouch, { ...adjustments, rotation: 3 })).toBe(false);
    expect(faceRetouchIsCurrent({ ...retouch, geometry: 'not json' }, adjustments)).toBe(false);
  });

  it('takes faces of another source frame as stale, by either stamp rule (ADR 0020)', () => {
    const adjustments = { ...INITIAL_ADJUSTMENTS, sourceFrameVersion: 0 } as Adjustments;
    for (const frame of ['2:x', 'x']) {
      const retouch = { settings: {}, faces: [], frame, geometry: frameGeometry(adjustments, frame) };
      expect(faceRetouchIsCurrent(retouch, adjustments)).toBe(true);
      expect(faceRetouchIsCurrent(retouch, { ...adjustments, sourceFrameVersion: 1 })).toBe(false);
    }
  });
});
