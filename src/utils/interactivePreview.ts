import { convertFileSrc } from '@tauri-apps/api/core';

/** Both reply formats retain the same six little-endian ROI dimensions. */
export function interactivePreview(buffer: ArrayBuffer) {
  const decoder = new TextDecoder();
  const uri = decoder.decode(buffer.slice(0, 11)) === 'JPEG_URI___';
  const offset = uri ? 11 : 0;
  if (buffer.byteLength < offset + 26) throw new Error('Incomplete preview packet');
  const view = new DataView(buffer, offset);
  const fullW = view.getUint32(16, true), fullH = view.getUint32(20, true);
  if (!fullW || !fullH) throw new Error('Invalid preview dimensions');
  let url: string;
  if (uri) {
    const id = decoder.decode(buffer.slice(offset + 24));
    if (!/^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$/i.test(id))
      throw new Error('Invalid preview frame ID');
    url = convertFileSrc(id, 'rawtonic-preview');
  } else url = URL.createObjectURL(new Blob([buffer.slice(24)], { type: 'image/jpeg' }));
  return {
    url,
    normX: view.getUint32(0, true) / fullW,
    normY: view.getUint32(4, true) / fullH,
    normW: view.getUint32(8, true) / fullW,
    normH: view.getUint32(12, true) / fullH,
  };
}
