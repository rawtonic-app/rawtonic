import { describe, expect, it } from 'vitest';
import { rustRecipeKeys } from '../../contracts/recipeKeys.testSupport';
import cacheUtils from '../../../crates/engine/src/cache_utils.rs?raw';
import faceRetouch from '../../../crates/engine/src/face_retouch.rs?raw';
import eyeTeeth from '../../../crates/engine/src/face_retouch/eye_teeth.rs?raw';
import {
  EYES_TEETH_VERSION,
  FACE_FRAME_KEYS,
  blemishThreshold,
  eyeChange,
  eyeValue,
  faceRetouchGeometry,
  removedBlemishes,
  withEyesAndMouth
} from './StudioFaceRetouch';
import type { RetouchFace } from '../../utils/adjustments';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';

const rustList = (source: string, start: RegExp) => {
  const body = source.slice(source.search(start));
  const list = body.slice(body.indexOf('[') + 1, body.indexOf(']'));
  return rustRecipeKeys(list);
};

describe('face retouch frame', () => {
  it('watches the same keys as the engine, so stale faces are found again', () => {
    const colourOnly = rustRecipeKeys(
      faceRetouch.match(/COLOUR_ONLY_KEYS: \[&str; \d+\] = \[([\s\S]*?)\];/)?.[1] ?? ''
    );
    expect(colourOnly).toContain('fringeVersion');
    const engine = [
      ...rustList(faceRetouch, /for key in \[/),
      ...rustRecipeKeys(cacheUtils.match(/GEOMETRY_KEYS: &\[&str\] = &\[([\s\S]*?)\];/)?.[1] ?? '')
    ].filter((key) => !colourOnly.includes(key));
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
        { id: 2, x: 0, y: 0, r: 1, score: 0.02 }
      ],
      kept: [] as number[],
      settings: {}
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

describe('eyes and teeth', () => {
  const values = (v: Record<string, number>) => (key: string) => v[key];

  it('shows one eye as the value for both plus its own offset, as the engine', () => {
    const get = values({ iris: 40, irisLeft: 30, scleraRight: -60 });
    expect(eyeValue(get, 'iris', 'both')).toBe(40);
    expect(eyeValue(get, 'iris', 'left')).toBe(70);
    expect(eyeValue(get, 'iris', 'right')).toBe(40);
    expect(eyeValue(values({ sclera: 20, scleraRight: -60 }), 'sclera', 'right')).toBe(0);
    expect(eyeValue(values({ iris: 90, irisLeft: 50 }), 'iris', 'left')).toBe(100);
  });

  it('stores one eye as an offset, so the value for both still moves it', () => {
    const get = values({ iris: 40 });
    expect(eyeChange(get, 'iris', 'left', 70)).toEqual({ key: 'irisLeft', value: 30 });
    expect(eyeChange(get, 'iris', 'right', 0)).toEqual({ key: 'irisRight', value: -40 });
    expect(eyeChange(values({}), 'sclera', 'right', 55)).toEqual({ key: 'scleraRight', value: 55 });
  });

  it('fills in the eyes and mouth of faces found before, from the face at the same place', () => {
    const face = (id: string, x: number, extra: Partial<RetouchFace> = {}): RetouchFace => ({
      id,
      crop: [1, 0, 0, 0, 1, 0],
      size: 10,
      eyeDistance: 10,
      skin: '',
      underEyes: null,
      blemishes: [],
      kept: [7],
      settings: { blemishes: 30 },
      enabled: true,
      box: [x, 0, 100, 100],
      ...extra
    });
    const eyes = [{ iris: [1, 2, 3], lid: [[0, 0]] }, null];
    const stored = [face('a', 0), face('b', 500, { eyes: [], mouth: null })];
    const analysed = [face('x', 10, { eyes, mouth: [[1, 1]] }), face('y', 900, { eyes, mouth: null })];
    const filled = withEyesAndMouth(stored, analysed);
    expect(filled[0]).toMatchObject({ id: 'a', eyes, mouth: [[1, 1]], kept: [7], settings: { blemishes: 30 } });
    // A face that has them keeps its own; none nearby: unchanged.
    expect(filled[1]).toBe(stored[1]);
    expect(withEyesAndMouth([face('c', 2000)], analysed)[0].eyes).toBeUndefined();
  });

  it("writes the engine's version", () => {
    expect(String(EYES_TEETH_VERSION)).toBe(
      eyeTeeth.match(/pub const EYES_TEETH_VERSION: u64 = (\d+);/)?.[1] ?? 'missing'
    );
  });
});
