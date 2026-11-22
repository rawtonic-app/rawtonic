import { describe, expect, it } from 'vitest';
import { rustRecipeKeys } from '../../contracts/recipeKeys.testSupport';
import cacheUtils from '../../../crates/engine/src/cache_utils.rs?raw';
import faceRetouch from '../../../crates/engine/src/face_retouch.rs?raw';
import {
  FACE_FRAME_KEYS,
  blemishThreshold,
  eyeChange,
  eyeValue,
  eyesSwapped,
  foundAgain,
  swapSides,
  faceRetouchGeometry,
  removedBlemishes,
  withEyesAndMouth,
  eyesTeethParsed,
  eyesTeethSet,
  withParsedEyesTeeth
} from './StudioFaceRetouch';
import type { FaceRetouch, RetouchFace } from '../../utils/adjustments';
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
      ...rustList(faceRetouch, /fn frame_keys\(/),
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
});

describe('eye and teeth parser outlines', () => {
  const face = (id: string, x: number, extra: Partial<RetouchFace> = {}): RetouchFace => ({
    id,
    crop: [1, 0, 0, 0, 1, 0],
    size: 10,
    eyeDistance: 10,
    skin: '',
    underEyes: null,
    blemishes: [],
    kept: [],
    settings: {},
    enabled: true,
    box: [x, 0, 100, 100],
    eyes: [{ iris: [1, 2, 3], lid: [[0, 0]] }, null],
    mouth: null,
    ...extra
  });
  const loop = [
    [
      [0, 0],
      [4, 0],
      [4, 4]
    ]
  ];
  const parsed = face('x', 10, {
    eyes: [{ iris: [1, 2, 3], lid: [[0, 0]], parsedEye: loop, parsedIris: loop }, null],
    mouth: [[1, 1]],
    parsedTeeth: loop
  });

  it('marks a retouch as parsed only with the teeth outlines', () => {
    expect(eyesTeethParsed([face('a', 0)])).toBe(false);
    expect(eyesTeethParsed([face('a', 0, { parsedTeeth: [] })])).toBe(true);
  });

  it('sees an Eyes or Teeth value for all faces or one', () => {
    const retouch = (settings: object, own: object = {}) => ({
      settings,
      faces: [face('a', 0, { settings: own })],
      frame: '',
      geometry: ''
    });
    expect(eyesTeethSet(retouch({ blemishes: 50 }))).toBe(false);
    expect(eyesTeethSet(retouch({ teethWhiten: 20 }))).toBe(true);
    expect(eyesTeethSet(retouch({}, { irisLeft: -10 }))).toBe(true);
    expect(eyesTeethSet(retouch({ iris: 0 }))).toBe(false);
  });

  it('takes the outlines of the face at the same place', () => {
    const [merged, far] = withParsedEyesTeeth([face('a', 0, { settings: { iris: 30 } }), face('b', 2000)], [parsed]);
    expect(merged).toMatchObject({ id: 'a', mouth: [[1, 1]], parsedTeeth: loop, settings: { iris: 30 } });
    expect(merged.eyes?.[0]?.parsedEye).toEqual(loop);
    expect(far.parsedTeeth).toBeUndefined();
  });
});

describe('eye sides through a flip', () => {
  const face = (left: number, right: number): RetouchFace =>
    ({
      id: 'f',
      box: [0, 0, 100, 100],
      settings: {},
      eyes: [
        { iris: [left, 50, 5], lid: [] },
        { iris: [right, 50, 5], lid: [] }
      ]
    }) as unknown as RetouchFace;
  const plain = { rotation: 0, flipHorizontal: false, flipVertical: false, orientationSteps: 0 };

  it('finds the eye on the left on the right after a horizontal flip', () => {
    const sides = { from: plain, to: { ...plain, flipHorizontal: true }, width: 200, height: 100 };
    // Mirrored: the eye at x 40 is at 160, the one at 80 at 120.
    expect(eyesSwapped(face(40, 80), face(120, 160), sides)).toBe(true);
    expect(eyesSwapped(face(40, 80), face(40, 80), { ...sides, to: plain })).toBe(false);
  });

  it('follows a lone eye through a flip', () => {
    const sides = { from: plain, to: { ...plain, flipHorizontal: true }, width: 200, height: 100 };
    const lone = (slot: 0 | 1, x: number) =>
      ({
        id: 'f',
        box: [x - 50, 0, 100, 100],
        settings: {},
        eyes: slot === 0 ? [{ iris: [x, 50, 5], lid: [] }, null] : [null, { iris: [x, 50, 5], lid: [] }]
      }) as unknown as RetouchFace;
    // The eye on the left at x 80 is at 120 after the flip: seen there as
    // the right one, or as the left one with the right one hidden.
    expect(eyesSwapped(lone(0, 80), lone(1, 120), sides)).toBe(true);
    expect(
      eyesSwapped(
        lone(0, 80),
        { ...face(120, 160), eyes: [{ iris: [120, 50, 5], lid: [] }, null] } as RetouchFace,
        sides
      )
    ).toBe(false);
  });

  it('exchanges only the left and right values', () => {
    expect(swapSides({ iris: 20, irisLeft: 30, scleraRight: -10, teethWhiten: 5 })).toEqual({
      iris: 20,
      irisRight: 30,
      scleraLeft: -10,
      teethWhiten: 5
    });
  });
});

describe('Iris disk through faces found again', () => {
  const parsedFace = (id: string, x: number): RetouchFace =>
    ({
      id,
      box: [x, 0, 100, 100],
      settings: {},
      parsedTeeth: [],
      eyes: [
        { iris: [x + 30, 50, 5], lid: [], parsedEye: [], parsedIris: [] },
        { iris: [x + 70, 50, 5], lid: [], parsedEye: [], parsedIris: [] }
      ]
    }) as unknown as RetouchFace;
  const straight = { ...INITIAL_ADJUSTMENTS };
  const straightened = { ...INITIAL_ADJUSTMENTS, rotation: 3 };
  const retouch = (irisDisk?: number): FaceRetouch => ({
    settings: { iris: 60 },
    faces: [parsedFace('a', 0), parsedFace('b', 300)],
    frame: '2:straight',
    geometry: JSON.stringify({}),
    ...(irisDisk !== undefined && { irisDisk })
  });
  // Straightened so far that face b leaves the frame, then straight again:
  // the parser's analyses carry the new disk.
  const roundTrip = (saved: FaceRetouch) => {
    const without = foundAgain(saved, { frame: '2:turned', faces: [parsedFace('a', 0)], irisDisk: 0.9 }, straightened);
    expect(without.faces).toHaveLength(1);
    return foundAgain(
      without,
      { frame: '2:straight', faces: [parsedFace('a', 0), parsedFace('b', 300)], irisDisk: 0.9 },
      straight
    );
  };

  it('keeps the full disk of a retouch made before it when a face leaves the frame and returns', () => {
    const back = roundTrip(retouch());
    expect(back.faces).toHaveLength(2);
    expect(back.settings).toEqual({ iris: 60 });
    expect(back.irisDisk).toBeUndefined();
  });

  it('keeps the disk a retouch was made with', () => {
    expect(roundTrip(retouch(0.85)).irisDisk).toBe(0.85);
  });
});
