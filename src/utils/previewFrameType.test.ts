import { describe, expect, it } from 'vitest';
import { previewFrameType } from './previewFrameType';

describe('previewFrameType', () => {
  it('names a PNG by its signature and everything else JPEG', () => {
    expect(previewFrameType(new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a]).buffer)).toBe('image/png');
    expect(previewFrameType(new Uint8Array([0xff, 0xd8, 0xff, 0xe0]).buffer)).toBe('image/jpeg');
    expect(previewFrameType(new Uint8Array([0x89, 0x50]).buffer)).toBe('image/jpeg');
    expect(previewFrameType(new ArrayBuffer(0))).toBe('image/jpeg');
  });
});
