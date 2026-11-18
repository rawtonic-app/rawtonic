/** The image type of a preview frame's bytes: a settled reduced frame comes
 * as a lossless PNG (Linux, `preview_jpeg::settled_lossless`), the others as
 * JPEG. */
export function previewFrameType(bytes: ArrayBuffer): 'image/png' | 'image/jpeg' {
  const head = new Uint8Array(bytes, 0, Math.min(4, bytes.byteLength));
  return head[0] === 0x89 && head[1] === 0x50 && head[2] === 0x4e && head[3] === 0x47 ? 'image/png' : 'image/jpeg';
}
