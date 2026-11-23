/**
 * DVL1: the graphics driver took the GPU device away (Windows' watchdog on
 * integrated GPUs) and the app started a new one (src-tauri gpu_recovery):
 * the open photo and the multi-view cells render again on it. Edits are
 * never affected: they live in the recipe, only the pictures are redrawn.
 */
import { listen } from '@tauri-apps/api/event';

export const GPU_DEVICE_RECOVERED = 'gpu-device-recovered';
export const GPU_DEVICE_LOST = 'gpu-device-lost';

/** What the app reports with `gpu-device-recovered`. */
export interface GpuRecovered {
  lost: number;
  reason: string;
  tookMs: number;
}

/** Calls `handler` after every recovery. Returns the stop. */
export function onGpuRecovered(handler: (recovered: GpuRecovered) => void): () => void {
  // Without the native app (tests, `npm run dev`) there is no event.
  const unlisten = listen<GpuRecovered>(GPU_DEVICE_RECOVERED, (event) => handler(event.payload)).catch(() => undefined);
  return () => void unlisten.then((stop) => stop?.());
}
