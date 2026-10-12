import { afterEach, expect, it, vi } from 'vitest';
import { convertFileSrc } from '@tauri-apps/api/core';
import { interactivePreview } from './interactivePreview';
vi.mock('@tauri-apps/api/core', () => ({ convertFileSrc: vi.fn((id, scheme) => `${scheme}://localhost/${id}`) }));
afterEach(() => { vi.unstubAllGlobals(); vi.clearAllMocks(); });
const id = '3145a106-8298-4bcc-8170-0186c856478b';
function packet(uri: boolean) {
  const prefix = uri ? new TextEncoder().encode('JPEG_URI___') : new Uint8Array();
  const tail = uri ? new TextEncoder().encode(id) : new Uint8Array([255, 216, 255, 217]);
  const bytes = new Uint8Array(prefix.length + 24 + tail.length);bytes.set(prefix);
  const view = new DataView(bytes.buffer, prefix.length);
  [20, 30, 40, 50, 200, 100].forEach((v, i) => view.setUint32(i * 4, v, true));
  bytes.set(tail, prefix.length + 24);return bytes.buffer;
}
it('preserves ROI placement across binary and direct-image replies', () => {
  const create = vi.fn(() => 'blob:frame');vi.stubGlobal('URL', { createObjectURL: create });
  expect(interactivePreview(packet(false))).toEqual({ url: 'blob:frame', normX: .1, normY: .3, normW: .2, normH: .5 });
  expect(interactivePreview(packet(true))).toEqual({ url: `rawtonic-preview://localhost/${id}`, normX: .1, normY: .3, normW: .2, normH: .5 });
  expect(create).toHaveBeenCalledOnce();expect(convertFileSrc).toHaveBeenCalledWith(id, 'rawtonic-preview');
});
it('rejects malformed metadata before creating an image URL', () => {
  expect(() => interactivePreview(new ArrayBuffer(4))).toThrow('Incomplete');
  const zero = packet(false);new DataView(zero).setUint32(16, 0, true);
  expect(() => interactivePreview(zero)).toThrow('dimensions');
  const invalid = new Uint8Array(packet(true));invalid[35] = 47;
  expect(() => interactivePreview(invalid.buffer)).toThrow('frame ID');
});
