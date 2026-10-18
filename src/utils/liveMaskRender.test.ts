import { expect, it, vi } from 'vitest';
import { INITIAL_ADJUSTMENTS, type Adjustments, type MaskContainer } from './adjustments';
import {
  forgetLiveStroke,
  liveStrokeFrame,
  registerLiveMaskRenderer,
  requestLiveMaskRender,
  withLiveMaskContainer,
} from './liveMaskRender';

const container = (id: string, lines: number): MaskContainer =>
  ({
    id,
    name: id,
    visible: true,
    invert: false,
    opacity: 100,
    adjustments: {},
    subMasks: [{ id: `${id}-brush`, type: 'brush', parameters: { lines: Array(lines).fill({}) } }],
  }) as unknown as MaskContainer;

it('replaces only the matching layer and keeps the committed adjustments intact', () => {
  const committed: Adjustments = { ...INITIAL_ADJUSTMENTS, masks: [container('a', 1), container('b', 2)] };
  const live = withLiveMaskContainer(committed, container('b', 3));
  expect(live.masks.map((mask) => mask.subMasks[0].parameters.lines!.length)).toEqual([1, 3]);
  expect(live.masks[0]).toBe(committed.masks[0]);
  expect(committed.masks[1].subMasks[0].parameters.lines).toHaveLength(2);
  expect(withLiveMaskContainer(committed, container('missing', 5))).toBe(committed);
});

it('routes live strokes to the newest renderer and ignores stale unregistration', () => {
  const first = vi.fn();
  const second = vi.fn();
  const unregisterFirst = registerLiveMaskRenderer(first);
  const unregisterSecond = registerLiveMaskRenderer(second);
  unregisterFirst();
  requestLiveMaskRender(INITIAL_ADJUSTMENTS);
  expect(first).not.toHaveBeenCalled();
  expect(second).toHaveBeenCalledTimes(1);
  unregisterSecond();
  requestLiveMaskRender(INITIAL_ADJUSTMENTS);
  expect(second).toHaveBeenCalledTimes(1);
});

it('sends a live stroke with the recipe once, then only its new points', () => {
  const points = (n: number) => Array.from({ length: n }, (_, i) => ({ x: i, y: 0 }));
  const stroke = (id: number, n: number) => ({ id, maskId: 'm', subMaskId: 's', points: points(n) });
  forgetLiveStroke();
  expect(liveStrokeFrame(stroke(1, 3))).toEqual({
    withRecipe: true,
    args: { id: 1, maskId: 'm', subMaskId: 's', from: 3, points: [] },
  });
  expect(liveStrokeFrame(stroke(1, 5))).toEqual({
    withRecipe: false,
    args: { id: 1, maskId: 'm', subMaskId: 's', from: 3, points: points(5).slice(3) },
  });
  // A frame dropped before sending changes nothing: the next one carries its points.
  expect(liveStrokeFrame(stroke(1, 9)).args).toMatchObject({ from: 5, points: points(9).slice(5) });
  // A new stroke, or a refusal, sends the recipe again.
  expect(liveStrokeFrame(stroke(2, 2)).withRecipe).toBe(true);
  forgetLiveStroke();
  expect(liveStrokeFrame(stroke(2, 4))).toMatchObject({ withRecipe: true, args: { from: 4, points: [] } });
});
