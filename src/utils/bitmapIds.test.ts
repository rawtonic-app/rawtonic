import { expect, it } from 'vitest';
import type { SubMask } from '../components/panel/right/Masks';
import { CACHED_BITMAP, forgetChangedMaskBitmaps, referToSentMaskBitmaps } from './bitmapIds';
import type { Adjustments } from './adjustments';

const sub = (id: string, maskDataBase64: string | null): SubMask =>
  ({
    id,
    type: 'ai-people',
    visible: true,
    mode: 'additive',
    parameters: { maskDataBase64, parts: [] }
  }) as unknown as SubMask;

it('refers to a sent bitmap, sends a new one whole and keeps a component without one empty', () => {
  const sent = new Set(['lips']);
  const lips = [sub('lips', 'data:lips'), sub('sky', 'data:sky')];
  expect(referToSentMaskBitmaps(lips, sent)).toEqual(['sky']);
  expect(lips[0].parameters.maskDataBase64).toBe(CACHED_BITMAP);
  expect(lips[1].parameters.maskDataBase64).toBe('data:sky');

  // Its last part unticked: no bitmap goes, so the native side renders none;
  // the next bitmap under the id goes whole.
  const unticked = [sub('lips', null)];
  expect(referToSentMaskBitmaps(unticked, sent)).toEqual([]);
  expect(unticked[0].parameters.maskDataBase64).toBeNull();
  expect(sent.has('lips')).toBe(false);
  const ticked = [sub('lips', 'data:teeth')];
  expect(referToSentMaskBitmaps(ticked, sent)).toEqual(['lips']);
  expect(ticked[0].parameters.maskDataBase64).toBe('data:teeth');
});

it('an undo that brings back another bitmap under the same id sends it whole', () => {
  const recipe = (...subs: SubMask[]) => ({ masks: [{ id: 'm', subMasks: subs }] }) as unknown as Adjustments;
  const sent = new Set(['land', 'sky', 'gone']);
  // Mountains + Water shown, Mountains-only restored; the sky is unchanged.
  forgetChangedMaskBitmaps(
    recipe(sub('land', 'data:mountains+water'), sub('sky', 'data:sky'), sub('gone', 'data:x')),
    recipe(sub('land', 'data:mountains'), sub('sky', 'data:sky')),
    sent
  );
  expect([...sent].sort()).toEqual(['gone', 'sky']);
  // A deleted layer brought back by History: its old bitmap is sent again.
  const back = new Set(['land']);
  forgetChangedMaskBitmaps(recipe(), recipe(sub('land', 'data:mountains')), back);
  expect(back.size).toBe(0);
  const restored = [sub('land', 'data:mountains')];
  expect(referToSentMaskBitmaps(restored, sent)).toEqual(['land']);
  expect(restored[0].parameters.maskDataBase64).toBe('data:mountains');
});
