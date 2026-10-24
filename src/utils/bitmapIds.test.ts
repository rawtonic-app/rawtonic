import { expect, it } from 'vitest';
import type { SubMask } from '../components/panel/right/Masks';
import { CACHED_BITMAP, referToSentMaskBitmaps } from './bitmapIds';

const sub = (id: string, maskDataBase64: string | null): SubMask =>
  ({ id, type: 'ai-people', visible: true, mode: 'additive', parameters: { maskDataBase64, parts: [] } }) as unknown as SubMask;

it('refers to a sent bitmap, sends a new one whole and keeps a component without one empty', () => {
  const sent = new Set(['lips']);
  const lips = [sub('lips', 'data:lips'), sub('sky', 'data:sky')];
  expect(referToSentMaskBitmaps(lips, sent)).toEqual(['sky']);
  expect(lips[0].parameters.maskDataBase64).toBe(CACHED_BITMAP);
  expect(lips[1].parameters.maskDataBase64).toBe('data:sky');

  // Its last part unticked: no bitmap goes, so the native side renders none
  // (and drops the cached one); the next bitmap under the id goes whole.
  const unticked = [sub('lips', null)];
  expect(referToSentMaskBitmaps(unticked, sent)).toEqual([]);
  expect(unticked[0].parameters.maskDataBase64).toBeNull();
  expect(sent.has('lips')).toBe(false);
  const ticked = [sub('lips', 'data:teeth')];
  expect(referToSentMaskBitmaps(ticked, sent)).toEqual(['lips']);
  expect(ticked[0].parameters.maskDataBase64).toBe('data:teeth');
});
