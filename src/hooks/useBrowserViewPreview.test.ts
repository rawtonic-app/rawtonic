import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { frameWidth, useBrowserViewPreview } from './useBrowserViewPreview';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
let complete: ((value: ArrayBuffer) => void)[];
const jpeg = () => {
  const buffer = new ArrayBuffer(12);
  const view = new DataView(buffer);
  view.setUint32(0, 6000, true);
  view.setUint32(4, 4000, true);
  return buffer;
};
beforeEach(() => {
  vi.useFakeTimers();
  complete = [];
  vi.mocked(invoke).mockImplementation(() => new Promise((resolve) => complete.push(resolve)));
  let sequence = 0;
  vi.stubGlobal('URL', {
    createObjectURL: vi.fn(() => `blob:cell-${++sequence}`),
    revokeObjectURL: vi.fn()
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});
const tick = () => act(() => vi.advanceTimersByTime(100));
const finish = (index: number) =>
  act(async () => {
    complete[index](jpeg());
  });

it('isolates secondary recipes, ignores a stale photo and releases its own URLs', async () => {
  const { result, rerender, unmount } = renderHook(({ path }) => useBrowserViewPreview(path, null, 1024), {
    initialProps: { path: '/first.raw' }
  });
  tick();
  expect(invoke).toHaveBeenCalledWith(Invokes.RenderBrowserView, {
    path: '/first.raw',
    jsAdjustments: null,
    targetResolution: 1024
  });
  rerender({ path: '/next.raw' });
  tick();
  await finish(1);
  expect(result.current.frame).toMatchObject({
    url: 'blob:cell-1',
    width: 6000,
    height: 4000
  });
  await finish(0);
  expect(result.current.frame?.url).toBe('blob:cell-1');
  expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
  unmount();
  expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:cell-1');
});

it('keeps a fit frame during zoom refinement but hides a superseded recipe', async () => {
  const { result, rerender } = renderHook(
    ({ recipe, resolution }) => useBrowserViewPreview('/photo.raw', recipe, resolution),
    { initialProps: { recipe: '{"exposure":0}', resolution: 1024 } }
  );
  tick();
  await finish(0);
  rerender({ recipe: '{"exposure":0}', resolution: 2048 });
  expect(result.current.frame?.url).toBe('blob:cell-1');
  tick();
  await finish(1);
  expect(result.current.frame?.url).toBe('blob:cell-2');
  expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:cell-1');
  rerender({ recipe: '{"exposure":1}', resolution: 2048 });
  expect(result.current.frame).toBeNull();
});

describe('frameWidth', () => {
  it('reads the width from the frame header after other segments', () => {
    // SOI, an APP0 of 4 bytes, SOF0 (precision 8, height 4000, width 6000).
    const bytes = new Uint8Array([
      0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x0f, 0xa0, 0x17, 0x70, 0x03
    ]);
    expect(frameWidth(bytes)).toBe(6000);
  });

  it('reads the width from a PNG header', () => {
    // Signature, IHDR length and type, width 2560, height 1707.
    const bytes = new Uint8Array([
      0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 0x0a, 0x00, 0, 0, 0x06,
      0xab
    ]);
    expect(frameWidth(bytes)).toBe(2560);
  });

  it('is 0 without a frame header', () => {
    expect(frameWidth(new Uint8Array([0xff, 0xd8, 0xff, 0xda, 0x00, 0x02, 0, 0, 0, 0]))).toBe(0);
    expect(frameWidth(new Uint8Array([1, 2, 3]))).toBe(0);
  });
});
