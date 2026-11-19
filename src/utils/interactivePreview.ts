import { convertFileSrc } from '@tauri-apps/api/core';

/** Leads a Linux settled frame at 100 % that covers the visible region only
 * (`editor_preview::BROWSER_REGION_MAGIC`): a lossless PNG of that region. */
const REGION_MAGIC = 'REGION_____';

export function isRegionFrame(buffer: ArrayBuffer) {
  return buffer.byteLength > 11 && new TextDecoder().decode(buffer.slice(0, 11)) === REGION_MAGIC;
}

/** Every reply format retains the same six little-endian ROI dimensions:
 * drag frames (bytes or a URI) and settled region frames. */
export function interactivePreview(buffer: ArrayBuffer) {
  const decoder = new TextDecoder();
  const magic = decoder.decode(buffer.slice(0, 11));
  const uri = magic === 'JPEG_URI___';
  const region = magic === REGION_MAGIC;
  const offset = uri || region ? 11 : 0;
  if (buffer.byteLength < offset + 26) throw new Error('Incomplete preview packet');
  const view = new DataView(buffer, offset);
  const fullW = view.getUint32(16, true),
    fullH = view.getUint32(20, true);
  if (!fullW || !fullH) throw new Error('Invalid preview dimensions');
  let url: string;
  if (uri) {
    const id = decoder.decode(buffer.slice(offset + 24));
    if (!/^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$/i.test(id))
      throw new Error('Invalid preview frame ID');
    url = convertFileSrc(id, 'rawtonic-preview');
  } else
    url = URL.createObjectURL(new Blob([buffer.slice(offset + 24)], { type: region ? 'image/png' : 'image/jpeg' }));
  return {
    url,
    normX: view.getUint32(0, true) / fullW,
    normY: view.getUint32(4, true) / fullH,
    normW: view.getUint32(8, true) / fullW,
    normH: view.getUint32(12, true) / fullH
  };
}
