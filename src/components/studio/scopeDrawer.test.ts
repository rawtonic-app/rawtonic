import { expect, it } from 'vitest';
import { createScopeDrawer, type ScopeTarget } from './scopeDrawer';

function setup() {
  const pending: Array<{ png: string; resolve: () => void; reject: () => void }> = [];
  const drawn: string[] = [];
  const closed: string[] = [];
  const bitmap = (png: string) => ({ png, close: () => closed.push(png) }) as unknown as ImageBitmap;
  const target: ScopeTarget = {
    clearRect: () => {},
    drawImage: (image) => drawn.push((image as unknown as { png: string }).png)
  };
  const draw = createScopeDrawer(
    (png) =>
      new Promise<ImageBitmap>((resolve, reject) =>
        pending.push({ png, resolve: () => resolve(bitmap(png)), reject: () => reject(new Error('bad png')) })
      ),
    () => target,
    256
  );
  const settle = () => new Promise((r) => setTimeout(r));
  return { pending, drawn, closed, draw, settle };
}

it('draws only the newest image when several arrive during one decode', async () => {
  const { pending, drawn, closed, draw, settle } = setup();
  draw('a');
  draw('b');
  draw('c');
  expect(pending.map((p) => p.png)).toEqual(['a']);
  pending[0].resolve();
  await settle();
  // 'a' is decoded but 'c' already waits: 'a' is dropped, 'b' never decoded.
  expect(drawn).toEqual([]);
  expect(pending.map((p) => p.png)).toEqual(['a', 'c']);
  pending[1].resolve();
  await settle();
  expect(drawn).toEqual(['c']);
  expect(closed).toEqual(['a', 'c']);
});

it('keeps the last image when a PNG does not decode, and goes on with the next', async () => {
  const { pending, drawn, draw, settle } = setup();
  draw('a');
  pending[0].resolve();
  await settle();
  draw('broken');
  pending[1].reject();
  await settle();
  expect(drawn).toEqual(['a']);
  draw('b');
  pending[2].resolve();
  await settle();
  expect(drawn).toEqual(['a', 'b']);
});
