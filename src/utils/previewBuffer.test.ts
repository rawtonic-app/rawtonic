import { afterEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { previewBuffer } from './previewBuffer';
const callbacks = vi.hoisted(() => [] as ((bytes: ArrayBuffer) => void)[]);
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(), Channel: class { constructor(receive: (bytes: ArrayBuffer) => void) { callbacks.push(receive); } },
}));
afterEach(() => { callbacks.length = 0;vi.clearAllMocks(); });
it('accepts a direct frame before the command acknowledgement', async () => {
  let acknowledge!: (bytes: ArrayBuffer) => void;
  vi.mocked(invoke).mockImplementation(() => new Promise((resolve) => { acknowledge = resolve; }));
  const result = previewBuffer('preview', {}, true);const bytes = new ArrayBuffer(71);
  callbacks[0](bytes);expect(await result).toBe(bytes);
  acknowledge(new ArrayBuffer(0));
});
it('retains binary replies and rejects superseded jobs', async () => {
  const bytes = new ArrayBuffer(50000);
  vi.mocked(invoke).mockResolvedValueOnce(bytes);
  expect(await previewBuffer('preview', {}, true)).toBe(bytes);
  vi.mocked(invoke).mockRejectedValueOnce('Superseded or worker failed');
  await expect(previewBuffer('preview', {}, true)).rejects.toBe('Superseded or worker failed');
});
it('creates no channel for ordinary requests', async () => {
  vi.mocked(invoke).mockResolvedValueOnce(new ArrayBuffer(2));
  await previewBuffer('preview', { sequence: 1 }, false);
  expect(callbacks).toHaveLength(0);expect(invoke).toHaveBeenCalledWith('preview', { sequence: 1 });
});
