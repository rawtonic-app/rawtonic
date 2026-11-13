import i18n from 'i18next';
import { sourceOf } from '../entities/photo/photoRef';

/** The backend's error when a photo exceeds the GPU's texture limit
 * (`rawtonic_gpu::gpu_processing::texture_limit_error`). */
const TEXTURE_LIMIT =
  /^The image \((\d+) × (\d+) px\) is larger than this graphics card can render \((\d+) px per side\)\.$/;

/** Export errors of src-tauri/src/export_processing.rs and export_queue.rs. */
const BATCH_FAILED = /^(\d+) of (\d+) exports failed$/;
const OFFLINE = /^(.+) is offline: exporting needs the RAW file$/;
const WRITE_FAILED = /^Failed to write file to '(.+)': (.+)$/s;
const KNOWN: Record<string, () => string> = {
  'Export task terminated unexpectedly': () => i18n.t('studio.export.errors.terminated'),
  // rawtonic_runtime::gpu_memory::GPU_MEMORY_EXHAUSTED: still out of memory after the export's retries.
  'Not enough graphics memory for this photo. Close other programs that use the graphics card and export this photo again.':
    () => i18n.t('studio.export.errors.gpuMemory'),
  // rawtonic_runtime::gpu_memory::GPU_DEVICE_LOST: the graphics chip's device was lost (G76/WCC6).
  'This photo could not be rendered on this graphics chip: it stopped responding (out of memory). Quit and reopen Rawtonic, then export this photo again.':
    () => i18n.t('studio.export.errors.gpuDeviceLost'),
  // rawtonic_runtime::gpu_memory::GPU_EXPORT_TOO_COMPLEX: an export variant over a compile-limited driver's ceiling (G76/WCC6).
  "This photo's edits are too complex for this graphics chip's driver to export. Remove some edits (effects, local adjustments) and export it again, or export it on another computer.":
    () => i18n.t('studio.export.errors.gpuTooComplex'),
  'Unreadable export request': () => i18n.t('studio.export.errors.unreadableRequest')
};

function fileName(path: string): string {
  const file = sourceOf(path);
  return file.slice(Math.max(file.lastIndexOf('/'), file.lastIndexOf('\\')) + 1);
}

/** A render or export error in the interface language; other errors keep their text. */
export function renderErrorMessage(error: unknown): string {
  const message = String(error).replace(/^Error: /, '');
  const limit = TEXTURE_LIMIT.exec(message);
  if (limit) {
    const [, width, height, max] = limit;
    return i18n.t('studio.export.errors.textureLimit', { width, height, limit: max });
  }
  const batch = BATCH_FAILED.exec(message);
  if (batch) {
    const [, failed, total] = batch;
    return i18n.t('studio.export.errors.batchFailed', { failed, total });
  }
  const offline = OFFLINE.exec(message);
  if (offline) return i18n.t('studio.export.errors.offline', { file: fileName(offline[1]) });
  const write = WRITE_FAILED.exec(message);
  if (write) {
    const [, path, reason] = write;
    return i18n.t('studio.export.errors.writeFailed', { file: fileName(path), error: reason });
  }
  return Object.hasOwn(KNOWN, message) ? KNOWN[message]() : message;
}
