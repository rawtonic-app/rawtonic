import { expect, it } from 'vitest';
import type { SubMask } from '../components/panel/right/Masks';
import { bitmapKey, cachedBitmapRef, referToSentMaskBitmaps } from './bitmapIds';

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
  expect(lips[0].parameters.maskDataBase64).toBe(cachedBitmapRef('data:lips'));
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

it('sends a bitmap whole again when it changed under its id (computed again, undo, redo)', () => {
  const sent = new Set<string>();
  const send = (bitmap: string) => {
    const subs = [sub('sky-again', bitmap)];
    referToSentMaskBitmaps(subs, sent).forEach((id) => sent.add(id));
    return subs[0].parameters.maskDataBase64;
  };
  expect(send('data:thumb')).toBe('data:thumb');
  expect(send('data:thumb')).toBe(cachedBitmapRef('data:thumb'));
  expect(send('data:full')).toBe('data:full');
  expect(send('data:full')).toBe(cachedBitmapRef('data:full'));
  expect(send('data:thumb')).toBe('data:thumb');
});

it('names the content it refers to, so another bitmap held under the id is never taken for it', () => {
  // A queued export may put the full-frame bitmap under the id after Undo
  // sent the stretched one: the preview's reference names the stretched one.
  const sent = new Set<string>();
  const send = (bitmap: string) => {
    const subs = [sub('sky-undo', bitmap)];
    referToSentMaskBitmaps(subs, sent).forEach((id) => sent.add(id));
    return subs[0].parameters.maskDataBase64;
  };
  send('data:full');
  send('data:thumb');
  expect(send('data:thumb')).toBe(`cached:${bitmapKey('data:thumb')}`);
  expect(cachedBitmapRef('data:thumb')).not.toBe(cachedBitmapRef('data:full'));
});

it('keys a bitmap as the native side does (bitmap_key in crates/engine/src/adjustment_utils.rs)', () => {
  expect(bitmapKey('')).toBe('3338908027751811');
  expect(bitmapKey('data:thumb')).toBe('7596472580574772');
  expect(bitmapKey('data:full')).toBe('6409932804508894');
  expect(bitmapKey('é✓😀')).toBe('2813155200082369');
});
