import { describe, expect, it } from 'vitest';
import cacheUtils from '../../../crates/engine/src/cache_utils.rs?raw';
import faceRetouch from '../../../crates/engine/src/face_retouch.rs?raw';
import { FACE_FRAME_KEYS, blemishThreshold, faceRetouchGeometry, removedBlemishes } from './StudioFaceRetouch';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';

const rustList = (source: string, start: RegExp) => {
  const body = source.slice(source.search(start));
  const list = body.slice(body.indexOf('[') + 1, body.indexOf(']'));
  return [...list.matchAll(/"([^"]+)"/g)].map((match) => match[1]);
};

describe('face retouch frame', () => {
  it('watches the same keys as the engine, so stale faces are found again', () => {
    const engine = [
      ...rustList(faceRetouch, /for key in \[/),
      ...[...(cacheUtils.match(/GEOMETRY_KEYS: &\[&str\] = &\[([\s\S]*?)\];/)?.[1] ?? '').matchAll(/"([^"]+)"/g)].map(
        (match) => match[1],
      ),
    ];
    expect([...FACE_FRAME_KEYS].sort()).toEqual(engine.sort());
  });

  it('counts the blemishes the engine heals', () => {
    // The engine's thresholds at 0, 50 and 100 (face_retouch.rs tests).
    expect(blemishThreshold(0)).toBe(Infinity);
    expect(blemishThreshold(1)).toBeCloseTo(0.015);
    expect(faceRetouch).toContain('const STRONG: f32 = 1.0;');
    expect(faceRetouch).toContain('const FAINT: f32 = 0.015;');
    expect(faceRetouch).toContain('(FAINT / STRONG).powf(amount.min(1.0).powf(0.25))');
    expect(blemishThreshold(0.3)).toBeCloseTo(0.045, 2);
    const face = {
      blemishes: [
        { id: 1, x: 0, y: 0, r: 1, score: 0.9 },
        { id: 2, x: 0, y: 0, r: 1, score: 0.02 },
      ],
      kept: [] as number[],
      settings: {},
    } as unknown as Parameters<typeof removedBlemishes>[0];
    expect(removedBlemishes(face, { blemishes: 20 })).toBe(1);
    expect(removedBlemishes(face, { blemishes: 100 })).toBe(2);
    expect(removedBlemishes(face, { blemishes: 100, impact: 20 })).toBe(1);
    expect(removedBlemishes({ ...face, kept: [1] }, { blemishes: 100 })).toBe(1);
    // Neck blemishes only with "include neck" (the engine's selected_blemishes).
    expect(faceRetouch).toContain('&& (neck || !b.neck)');
    const withNeck = { ...face, blemishes: [...face.blemishes, { id: 3, x: 0, y: 0, r: 1, score: 0.9, neck: true }] };
    expect(removedBlemishes(withNeck, { blemishes: 100 })).toBe(2);
    expect(removedBlemishes(withNeck, { blemishes: 100, includeNeck: true })).toBe(3);
    expect(removedBlemishes({ ...withNeck, settings: { includeNeck: true } }, { blemishes: 100 })).toBe(3);
  });

  it('leaves out unset keys and everything else', () => {
    const geometry = faceRetouchGeometry({ ...INITIAL_ADJUSTMENTS, exposure: 1, lensMaker: null });
    expect(geometry).not.toHaveProperty('exposure');
    expect(geometry).not.toHaveProperty('lensMaker');
    expect(geometry).toHaveProperty('rotation');
  });
});
