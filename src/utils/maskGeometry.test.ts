import { describe, expect, it } from 'vitest';
import {
  frameGeometry,
  keepDrawnMasksOnPhoto,
  frameDrawnMasks,
  photoPixelToShown,
  photoToShown,
  reframePoint,
  sameFrameGeometry,
  shapeAsShown,
  shapeFrame,
  shownPixelToPhoto,
  shownToPhoto,
} from './maskGeometry';

const W = 600;
const H = 400;

describe('mask geometry', () => {
  const geometries = [
    frameGeometry({}),
    frameGeometry({ orientationSteps: 1 }),
    frameGeometry({ orientationSteps: 2, flipHorizontal: true }),
    frameGeometry({ orientationSteps: 3, flipVertical: true, rotation: 7.5 }),
    frameGeometry({ orientationSteps: 1, rotation: -12, flipHorizontal: true, flipVertical: true }),
  ];

  it('maps a shown point to the photo and back', () => {
    for (const g of geometries) {
      for (const p of [{ x: 10, y: 20 }, { x: 300, y: 150 }, { x: 590, y: 390 }]) {
        const shown = photoToShown(p, g, W, H);
        const back = shownToPhoto(shown, g, W, H);
        expect(back.x).toBeCloseTo(p.x, 6);
        expect(back.y).toBeCloseTo(p.y, 6);
      }
    }
  });

  it('turns a quarter clockwise like the image (rotate90)', () => {
    // The photo's top left corner is shown at the top right.
    const g = frameGeometry({ orientationSteps: 1 });
    expect(photoToShown({ x: 0, y: 0 }, g, W, H)).toEqual({ x: H, y: 0 });
    expect(photoToShown({ x: W, y: H }, g, W, H)).toEqual({ x: 0, y: W });
  });

  it('keeps a sample on its spot of the photo when the photo turns later', () => {
    const taken = frameGeometry({ orientationSteps: 1, rotation: 5 });
    const now = frameGeometry({ orientationSteps: 2, flipVertical: true });
    const spot = { x: 120, y: 330 };
    const sample = photoToShown(spot, taken, W, H);
    const shownNow = reframePoint(sample, taken, now, W, H);
    const expected = photoToShown(spot, now, W, H);
    expect(shownNow.x).toBeCloseTo(expected.x, 6);
    expect(shownNow.y).toBeCloseTo(expected.y, 6);
    expect(reframePoint(sample, taken, taken, W, H)).toBe(sample);
  });

  it('reads missing geometry as the photo shown as it is', () => {
    expect(sameFrameGeometry(frameGeometry(undefined), frameGeometry({ orientationSteps: 4 }))).toBe(true);
    expect(frameGeometry({ orientationSteps: -1 }).orientationSteps).toBe(3);
  });
});

describe('drawn mask frames', () => {
  const upright = shapeFrame({}, W, H)!;
  const turned = shapeFrame({ orientationSteps: 1 }, W, H)!;
  const flipped = shapeFrame({ flipHorizontal: true, rotation: 8 }, W, H)!;
  const close = (a: { x: number; y: number }, b: { x: number; y: number }) => {
    expect(a.x).toBeCloseTo(b.x, 6);
    expect(a.y).toBeCloseTo(b.y, 6);
  };
  /** The pixel of the photo under a pixel shown in `frame` (as the engine
   * places drawn masks). */
  const onPhoto = (p: { x: number; y: number }, frame: typeof upright) => shownPixelToPhoto(p, frame, W, H);

  it('moves a radial drawn before a turn onto the same spot of the photo', () => {
    const drawn = { centerX: 200, centerY: 120, radiusX: 80, radiusY: 30, rotation: 25, feather: 0.5, frame: upright };
    for (const now of [turned, flipped]) {
      const shown = shapeAsShown('radial', drawn, now);
      expect(shown.frame).toEqual(now);
      expect([shown.radiusX, shown.radiusY, shown.feather]).toEqual([80, 30, 0.5]);
      close(onPhoto({ x: shown.centerX, y: shown.centerY }, now), onPhoto({ x: 200, y: 120 }, upright));
      // The end of its long axis stays on its spot too.
      const axis = (p: typeof drawn, r: number) => ({
        x: p.centerX + r * Math.cos((p.rotation * Math.PI) / 180),
        y: p.centerY + r * Math.sin((p.rotation * Math.PI) / 180),
      });
      const end = onPhoto(axis(drawn, 80), upright);
      const shownEnd = onPhoto(axis(shown, 80), now);
      const shownOther = onPhoto(axis(shown, -80), now);
      // A mirror may swap the axis' ends: either end lands there.
      const d = Math.min(
        Math.hypot(shownEnd.x - end.x, shownEnd.y - end.y),
        Math.hypot(shownOther.x - end.x, shownOther.y - end.y),
      );
      expect(d).toBeLessThan(1e-6);
    }
  });

  it('keeps each side of a linear gradient on its part of the photo, mirrored too', () => {
    const drawn = { startX: 100, startY: 300, endX: 400, endY: 120, range: 40, rangeEnd: 15, frame: upright };
    // Signed distance from the central line (negative: the 100% side).
    const side = (p: { startX: number; startY: number; endX: number; endY: number }, q: { x: number; y: number }) => {
      const len = Math.hypot(p.endX - p.startX, p.endY - p.startY);
      return ((q.x - p.startX) * -(p.endY - p.startY) + (q.y - p.startY) * (p.endX - p.startX)) / len;
    };
    const spots = [
      { x: 150, y: 150 },
      { x: 420, y: 350 },
    ];
    for (const now of [turned, flipped, shapeFrame({ flipVertical: true, orientationSteps: 3 }, W, H)!]) {
      const shown = shapeAsShown('linear', drawn, now);
      expect([shown.range, shown.rangeEnd]).toEqual([40, 15]);
      for (const spot of spots) {
        const before = side(drawn, photoPixelToShown(spot, upright, W, H));
        const after = side(shown, photoPixelToShown(spot, now, W, H));
        expect(after).toBeCloseTo(before, 6);
      }
    }
  });

  it('shows strokes painted in other frames where they were painted, each by its own frame', () => {
    const before = { tool: 'brush', brushSize: 20, points: [{ x: 10, y: 20 }, { x: 50, y: 60 }], frame: upright };
    const after = { tool: 'brush', brushSize: 20, points: [{ x: 30, y: 40 }], frame: turned };
    const older = { tool: 'eraser', brushSize: 5, points: [{ x: 7, y: 9 }] };
    const params = { lines: [before, after, older] };
    const shown = shapeAsShown('brush', params, turned);
    const lines = shown.lines as Array<typeof before>;
    lines[0].points.forEach((p, i) => close(onPhoto(p, turned), onPhoto(before.points[i], upright)));
    expect(lines[0].brushSize).toBe(20);
    expect(lines[1]).toBe(after);
    expect(lines[2]).toBe(older);
    // Shown as painted: nothing changes.
    expect(shapeAsShown('brush', { lines: [after, older] }, turned)).toEqual({ lines: [after, older] });
  });

  it('leaves shapes drawn as the photo is shown, and older ones, in place', () => {
    const radial = { centerX: 200, centerY: 120, radiusX: 80, radiusY: 30, rotation: 25 };
    expect(shapeAsShown('radial', radial, turned)).toEqual({ ...radial, frame: turned });
    expect(shapeAsShown('radial', { ...radial, frame: turned }, turned)).toEqual({ ...radial, frame: turned });
    expect(shapeAsShown('radial', radial, undefined)).toBe(radial);
    expect(shapeAsShown('ai-subject', radial, turned)).toBe(radial);
    // A frame without a size takes the photo's; an unreadable one leaves
    // the shape as stored.
    const sizeless = shapeAsShown('radial', { ...radial, frame: { orientationSteps: 1 } }, upright);
    expect(sizeless).toEqual(shapeAsShown('radial', { ...radial, frame: turned }, upright));
    const broken = { ...radial, frame: { width: -1, height: 4 } };
    expect(shapeAsShown('radial', broken, upright)).toEqual({ ...broken, frame: upright });
  });

  it('places shapes as the engine does', () => {
    // The numbers of the engine's `shapes_move_as_the_viewer_moves_them`.
    const drawn = { width: 60, height: 40, rotation: 7, flipHorizontal: true, flipVertical: false, orientationSteps: 1 };
    const now = shapeFrame({ rotation: -3, orientationSteps: 2 }, 90, 50)!;
    const radial = shapeAsShown(
      'radial',
      { centerX: 12, centerY: 31, radiusX: 9, radiusY: 4, rotation: 20, feather: 0.5, frame: drawn },
      now,
    );
    const linear = shapeAsShown(
      'linear',
      { startX: 5, startY: 8, endX: 30, endY: 50, range: 6, rangeEnd: 2, band: { width: 12, feather: 3 }, frame: drawn },
      now,
    );
    const stroke = shapeAsShown('brush', { lines: [{ brushSize: 10, points: [{ x: 12, y: 31 }], frame: drawn }] }, now);
    const expected: Array<[unknown, number]> = [
      [radial.centerX, 41.257114],
      [radial.centerY, 33.856149],
      [radial.radiusX, 11.374553],
      [radial.radiusY, 5.934299],
      [radial.rotation, -108.484957],
      [linear.startX, 14.974016],
      [linear.startY, 9.972283],
      [linear.endX, 74.813051],
      [linear.endY, 44.302807],
      [linear.range, 7.970568],
      [linear.rangeEnd, 2.656856],
      [linear.band.width, 15.941136],
      [linear.band.feather, 3.985284],
      [stroke.lines[0].points[0].x, 41.257114],
      [stroke.lines[0].points[0].y, 33.856149],
      [stroke.lines[0].brushSize, 10 * Math.sqrt(1.875)],
    ];
    for (const [got, want] of expected) expect(got as number).toBeCloseTo(want, 5);
    expect(radial.frame).toEqual(now);
  });

  it('puts a mask copied to a photo of another size on the same part of it', () => {
    const small = shapeFrame({}, 300, 200)!;
    const large = shapeFrame({ orientationSteps: 1 }, 600, 400)!;
    const drawn = { centerX: 149.5, centerY: 49.5, radiusX: 40, radiusY: 20, rotation: 0, frame: small };
    const shown = shapeAsShown('radial', drawn, large);
    // The centre at the middle of the top quarter of the photo, now turned.
    const centre = shownPixelToPhoto({ x: shown.centerX, y: shown.centerY }, large, 600, 400);
    expect(centre.x).toBeCloseTo(299.5, 6);
    expect(centre.y).toBeCloseTo(99.5, 6);
    expect(shown.radiusX).toBeCloseTo(80, 6);
    expect(shown.radiusY).toBeCloseTo(40, 6);
    // The same size, by less than a pixel: as drawn.
    const same = shapeFrame({}, 300.4, 200)!;
    expect(shapeAsShown('radial', drawn, same)).toEqual({ ...drawn, frame: same });
  });

  it('gives masks leaving their photo its frame and size', () => {
    const geometry = { rotation: 2, orientationSteps: 3 };
    const masks = [
      {
        subMasks: [
          { type: 'radial', parameters: { centerX: 1 } },
          { type: 'linear', parameters: { startX: 1, frame: { orientationSteps: 1 } } },
          { type: 'brush', parameters: { lines: [{ points: [] }, { points: [], frame: turned }] } },
          { type: 'ai-sky', parameters: { maskDataBase64: 'x' } },
        ],
      },
    ];
    const framed = frameDrawnMasks(masks, geometry, W, H)!;
    const [radial, linear, brush, ai] = framed[0].subMasks;
    expect(radial.parameters.frame).toEqual(shapeFrame(geometry, W, H));
    expect(linear.parameters.frame).toEqual({ orientationSteps: 1, width: W, height: H });
    expect((brush.parameters.lines as Array<{ frame?: unknown }>).map((l) => l.frame)).toEqual([
      shapeFrame(geometry, W, H),
      turned,
    ]);
    expect(ai).toBe(masks[0].subMasks[3]);
    // Without the size: the geometry alone; nothing to do keeps the objects.
    expect(frameDrawnMasks(masks, geometry, undefined, undefined)![0].subMasks[0].parameters.frame).toEqual(
      frameGeometry(geometry),
    );
    expect(frameDrawnMasks(framed, geometry, W, H)).toBe(framed);
  });

  it('gives older drawn masks the frame they were shown in when an edit turns the photo', () => {
    const radial = { id: 'r', type: 'radial', parameters: { centerX: 1, centerY: 2, radiusX: 3, radiusY: 4 } };
    const brush = {
      id: 'b',
      type: 'brush',
      parameters: { lines: [{ tool: 'brush', points: [], frame: turned }, { tool: 'brush', points: [] }] },
    };
    const ai = { id: 'a', type: 'ai-sky', parameters: { maskDataBase64: 'x' } };
    type Recipe = {
      rotation?: number;
      orientationSteps?: number;
      exposure?: number;
      masks: Array<{ subMasks: Array<{ id: string; type: string; parameters: Record<string, unknown> }> }>;
      aiPatches: Array<{ subMasks: Array<{ id: string; type: string; parameters: Record<string, unknown> }> }>;
    };
    const previous: Recipe = { rotation: 3, masks: [{ subMasks: [radial, brush, ai] }], aiPatches: [] };
    const next = keepDrawnMasksOnPhoto(previous, { ...previous, orientationSteps: 1 }, W, H);
    const was = shapeFrame({ rotation: 3 }, W, H);
    const [r, b, a] = next.masks[0].subMasks;
    expect(r.parameters).toEqual({ ...radial.parameters, frame: was });
    expect((b.parameters.lines as Array<{ frame?: unknown }>).map((l) => l.frame)).toEqual([turned, was]);
    expect(a).toBe(ai);
    expect(next.aiPatches).toBe(previous.aiPatches);
    // No change of geometry: nothing to do. An unknown size: the frame
    // records none (the engine takes the photo's).
    const same = { ...previous, exposure: 1 };
    expect(keepDrawnMasksOnPhoto(previous, same, W, H)).toBe(same);
    const turnedNow = keepDrawnMasksOnPhoto(previous, { ...previous, orientationSteps: 1 }, 0, 0);
    expect(turnedNow.masks[0].subMasks[0].parameters.frame).toEqual(frameGeometry({ rotation: 3 }));
    // Masks the edit brings along (pasted, restored) are not the ones shown.
    const pasted: Recipe = {
      ...previous,
      orientationSteps: 1,
      masks: [{ subMasks: [{ ...radial, parameters: { ...radial.parameters } }] }],
    };
    expect(keepDrawnMasksOnPhoto(previous, pasted, W, H)).toBe(pasted);
  });
});
