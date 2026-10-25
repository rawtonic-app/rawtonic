import { describe, expect, it } from 'vitest';
import {
  nextPrompts,
  promptBounds,
  promptGesture,
  promptFrameKey,
  promptOf,
  promptsFrameChanged,
  storedPrompts,
  type ObjectPrompt,
} from './objectPrompts';

const at = (x: number, y: number) => ({ x, y });
const upright = { rotation: 0, flipHorizontal: false, flipVertical: false, orientationSteps: 0 };
// As the backend stores it: the lens and transform adjustments the mask was made in.
const warp = { transformVertical: 0, lensCorrectionMode: 'auto' };
const shown = { ...upright, ...warp, exposure: 1 };

describe('Object selection prompts', () => {
  it('reads Shift as adding, Alt as excluding (Alt wins), a plain press as starting over', () => {
    expect(promptGesture({})).toBe('replace');
    expect(promptGesture({ shiftKey: true })).toBe('add');
    expect(promptGesture({ altKey: true })).toBe('exclude');
    expect(promptGesture({ shiftKey: true, altKey: true })).toBe('exclude');
  });

  it('makes a click of a press that stays put, else a box', () => {
    expect(promptOf(at(5, 6), at(5, 6))).toEqual({ type: 'point', x: 5, y: 6 });
    expect(promptOf(at(5, 6), at(50, 60), true)).toEqual({
      type: 'box',
      startX: 5,
      startY: 6,
      endX: 50,
      endY: 60,
      exclude: true,
    });
  });

  it('adds with Shift, excludes with Alt and starts over with a plain press', () => {
    const first = promptOf(at(10, 10), at(10, 10));
    let prompts = nextPrompts([], first, 'replace')!;
    expect(prompts).toEqual([first]);
    prompts = nextPrompts(prompts, promptOf(at(100, 100), at(200, 180)), 'add')!;
    prompts = nextPrompts(prompts, promptOf(at(150, 150), at(150, 150), true), 'exclude')!;
    expect(prompts).toEqual([
      { type: 'point', x: 10, y: 10 },
      { type: 'box', startX: 100, startY: 100, endX: 200, endY: 180 },
      { type: 'point', x: 150, y: 150, exclude: true },
    ]);
    expect(nextPrompts(prompts, promptOf(at(1, 2), at(1, 2)), 'replace')).toEqual([{ type: 'point', x: 1, y: 2 }]);
  });

  it('takes nothing out of an empty selection', () => {
    expect(nextPrompts([], promptOf(at(1, 2), at(1, 2), true), 'exclude')).toBeNull();
  });

  it('keeps startX…endY as the first included prompt', () => {
    expect(
      promptBounds([
        { type: 'point', x: 3, y: 3, exclude: true },
        { type: 'box', startX: 1, startY: 2, endX: 3, endY: 4 },
        { type: 'point', x: 9, y: 9 },
      ]),
    ).toEqual({ start: at(1, 2), end: at(3, 4) });
    expect(promptBounds([{ type: 'point', x: 7, y: 8 }])).toEqual({ start: at(7, 8), end: at(7, 8) });
  });

  it("builds on a recipe's one box or click from before", () => {
    const params = { maskDataBase64: 'png', startX: 1, startY: 2, endX: 30, endY: 40, ...upright, warp };
    expect(storedPrompts(params, shown)).toEqual([{ type: 'box', startX: 1, startY: 2, endX: 30, endY: 40 }]);
    expect(storedPrompts({ ...params, endX: 1, endY: 2 }, shown)).toEqual([{ type: 'point', x: 1, y: 2 }]);
  });

  it('builds on nothing without a mask (pasted, waiting) or after the photo was turned or warped', () => {
    const prompts: ObjectPrompt[] = [{ type: 'point', x: 1, y: 1 }];
    const params = { maskDataBase64: 'png', prompts, startX: 1, startY: 1, endX: 1, endY: 1, ...upright, warp };
    expect(storedPrompts(params, shown)).toEqual(prompts);
    expect(storedPrompts({ ...params, maskDataBase64: null }, shown)).toEqual([]);
    expect(storedPrompts(params, { ...shown, orientationSteps: 1 })).toEqual([]);
    expect(storedPrompts(params, { ...shown, rotation: 2 })).toEqual([]);
    // Perspective, offsets, lens: the stored points sit elsewhere on the photo now.
    expect(storedPrompts(params, { ...shown, transformVertical: 12 })).toEqual([]);
    expect(storedPrompts(params, { ...shown, lensDistortionAmount: 40 })).toEqual([]);
    // A mask from before `warp` was stored.
    const { warp: _warp, ...withoutWarp } = params;
    expect(storedPrompts(withoutWarp, shown)).toEqual([]);
  });

  it('keeps the prompts across lens corrections that move nothing; refuses adding after a turn or warp', () => {
    const prompts: ObjectPrompt[] = [{ type: 'point', x: 1, y: 1 }];
    const params = { maskDataBase64: 'png', prompts, ...upright, warp };
    expect(storedPrompts(params, { ...shown, lensVignetteAmount: 50, lensTcaEnabled: false })).toEqual(prompts);
    expect(promptsFrameChanged(params, { ...shown, lensVignetteAmount: 50 })).toBe(false);
    expect(promptsFrameChanged(params, { ...shown, flipHorizontal: true })).toBe(true);
    expect(promptsFrameChanged(params, { ...shown, transformXOffset: 3 })).toBe(true);
    // Nothing to add to: not a changed frame.
    expect(promptsFrameChanged({ ...params, maskDataBase64: null }, { ...shown, rotation: 4 })).toBe(false);
  });

  it('gives the same frame key for the same frame whatever the key order, another for a changed warp', () => {
    expect(promptFrameKey({ ...upright, guidedPerspective: { a: 1, b: [2, 3] } })).toBe(
      promptFrameKey({ guidedPerspective: { b: [2, 3], a: 1 }, ...upright }),
    );
    expect(promptFrameKey(shown)).not.toBe(promptFrameKey({ ...shown, transformXOffset: 5 }));
  });
});
