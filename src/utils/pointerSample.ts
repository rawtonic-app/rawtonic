import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import { Invokes } from '../components/ui/AppProperties';

/** The shown frame's display pixels around the pointer (`sample_preview_patch`
 * in src-tauri/src/gpu_processing.rs): at most 11 × 11 cells, sRGB 0–255. */
export interface PreviewPatch {
  /** Where the cells start in the full 11 × 11 block (clipped at edges). */
  column: number;
  row: number;
  width: number;
  height: number;
  /** Row-major R, G, B per cell. */
  pixels: number[];
  /** The pointer's cell, in patch coordinates. */
  center: [number, number];
  /** The colour pickers' value there (mean of the 11 × 11 block); null at
   * the edge of a zoomed frame's rendered part. */
  mean: [number, number, number] | null;
  /** The display frame's size in pixels. */
  frame: [number, number];
}

export const PATCH_SIZE = 11;

export type ReadoutUnit = 'rgb255' | 'percent';

/** The pointer's place on the displayed photograph, as the pickers
 * normalise it: 0–1 across the photograph as shown (its crop, rotation,
 * orientation and warp are already in the shown frame), independent of zoom
 * and of the screen's pixel density (both sides are CSS pixels). `null`
 * outside the photograph. */
export function pointerToNormalized(
  clientX: number,
  clientY: number,
  rect: { left: number; top: number; width: number; height: number },
): { x: number; y: number } | null {
  if (!(rect.width > 0 && rect.height > 0)) return null;
  const x = (clientX - rect.left) / rect.width;
  const y = (clientY - rect.top) / rect.height;
  if (!(x >= 0 && x < 1 && y >= 0 && y < 1)) return null;
  return { x, y };
}

/** The display-frame pixel a normalised point falls on (`sample_center` in
 * crates/gpu): the loupe's centre cell and the pickers' sample centre. */
export function frameTexel(x: number, y: number, width: number, height: number): [number, number] {
  return [Math.min(Math.floor(x * width), width - 1), Math.min(Math.floor(y * height), height - 1)];
}

const round1 = (value: number) => Math.round(value * 10) / 10;

/** R, G and B as the readout shows them: 0–255 or percent (one decimal). */
export function readoutValues(rgb: readonly number[], unit: ReadoutUnit): string[] {
  return rgb.slice(0, 3).map((value) =>
    unit === 'percent' ? round1((value / 255) * 100).toFixed(1) : String(Math.round(value)),
  );
}

/** The patch's cell at `[column, row]` of the 11 × 11 block, or null when the
 * frame did not render it. */
export function patchCell(patch: PreviewPatch, column: number, row: number): [number, number, number] | null {
  const c = column - patch.column;
  const r = row - patch.row;
  if (c < 0 || r < 0 || c >= patch.width || r >= patch.height) return null;
  const i = (r * patch.width + c) * 3;
  return [patch.pixels[i], patch.pixels[i + 1], patch.pixels[i + 2]];
}

/** The value the readout shows: the pickers' 11 × 11 mean when the frame
 * rendered the whole block, else the pointer's own cell. */
export function readoutColor(patch: PreviewPatch): [number, number, number] | null {
  if (patch.mean) return patch.mean;
  return patchCell(patch, patch.column + patch.center[0], patch.row + patch.center[1]);
}

/** Where the loupe goes: below right of the pointer with a gap, so it never
 * covers the sampled pixel; flipped to the other side near the viewer's
 * right or bottom edge. Coordinates are relative to the viewer's box. */
export function loupePlacement(
  pointer: { x: number; y: number },
  loupe: { width: number; height: number },
  viewer: { width: number; height: number },
  gap = 24,
): { left: number; top: number } {
  const right = pointer.x + gap;
  const below = pointer.y + gap;
  const left = right + loupe.width <= viewer.width ? right : pointer.x - gap - loupe.width;
  const top = below + loupe.height <= viewer.height ? below : pointer.y - gap - loupe.height;
  return { left: Math.max(0, left), top: Math.max(0, top) };
}

export interface PointerSample {
  /** Pointer position in the window (CSS px), for the loupe. */
  clientX: number;
  clientY: number;
  patch: PreviewPatch | null;
}

interface PointerSampleState {
  sample: PointerSample | null;
  set(sample: PointerSample | null): void;
}

/** The last sample under the pointer, shared by the loupe and the viewer
 * bar's readout. */
export const usePointerSampleStore = create<PointerSampleState>((set) => ({
  sample: null,
  set: (sample) => set({ sample }),
}));

/** `inputAt`: the pointer event's time stamp (performance.now() base). */
type Request = { path: string; x: number; y: number; clientX: number; clientY: number; inputAt?: number };
export type Fetch = (path: string, x: number, y: number) => Promise<PreviewPatch | null>;

const fetchPatch: Fetch = (path, x, y) =>
  invoke<PreviewPatch | null>(Invokes.SamplePreviewPatch, { path, normalizedX: x, normalizedY: y });

/** The renderer's picker block (`preview_sample_roi` in crates/gpu): 11 × 11
 * pixels centred on the point, clipped at the image's edges. */
export function sampleBlock(x: number, y: number, width: number, height: number) {
  const centerX = Math.floor(x * width);
  const centerY = Math.floor(y * height);
  const left = Math.max(0, centerX - 5);
  const top = Math.max(0, centerY - 5);
  return { left, top, width: Math.min(centerX + 6, width) - left, height: Math.min(centerY + 6, height) - top };
}

/** A patch from 8-bit sRGB RGBA pixels of a whole `width` × `height` image
 * (the browser-rendered preview on Linux, where there is no native frame):
 * the same block, centre and mean as the native `sample_display_patch`. */
export function patchFromRgba(
  read: (left: number, top: number, width: number, height: number) => ArrayLike<number>,
  width: number,
  height: number,
  x: number,
  y: number,
): PreviewPatch | null {
  if (!(width > 0 && height > 0) || !(x >= 0 && x <= 1 && y >= 0 && y <= 1)) return null;
  const block = sampleBlock(x, y, width, height);
  if (block.width <= 0 || block.height <= 0) return null;
  const rgba = read(block.left, block.top, block.width, block.height);
  const count = block.width * block.height;
  const pixels: number[] = [];
  const sum = [0, 0, 0];
  for (let i = 0; i < count; i += 1) {
    for (let c = 0; c < 3; c += 1) {
      pixels.push(rgba[i * 4 + c]);
      sum[c] += rgba[i * 4 + c];
    }
  }
  const [cx, cy] = frameTexel(x, y, width, height);
  return {
    column: 0,
    row: 0,
    width: block.width,
    height: block.height,
    pixels,
    center: [cx - block.left, cy - block.top],
    mean: [sum[0] / count, sum[1] / count, sum[2] / count],
    frame: [width, height],
  };
}

/** Patches of the browser-rendered preview at `url()` (the image the white
 * balance picker's fallback samples). Only the newest image is kept. */
export function createImagePatchFetch(url: () => string | null): Fetch {
  let loaded: { url: string; image: Promise<HTMLImageElement> } | null = null;
  const canvas = typeof document !== 'undefined' ? document.createElement('canvas') : null;
  canvas?.setAttribute('width', String(PATCH_SIZE));
  canvas?.setAttribute('height', String(PATCH_SIZE));
  const context = canvas?.getContext('2d', { willReadFrequently: true }) ?? null;
  return async (_path, x, y) => {
    const current = url();
    if (!current || !context) return null;
    if (loaded?.url !== current) {
      const image = new Image();
      image.crossOrigin = 'Anonymous';
      image.src = current;
      loaded = { url: current, image: image.decode().then(() => image) };
    }
    const image = await loaded.image;
    return patchFromRgba(
      (left, top, width, height) => {
        context.clearRect(0, 0, PATCH_SIZE, PATCH_SIZE);
        context.drawImage(image, left, top, width, height, 0, 0, width, height);
        return context.getImageData(0, 0, width, height).data;
      },
      image.naturalWidth,
      image.naturalHeight,
      x,
      y,
    );
  };
}

/** Pointer moves → samples, coalesced: at most one request in flight; moves
 * that arrive meanwhile are dropped but the newest, which is sent when the
 * answer comes. WebKit delivers pointer moves at most once per display frame,
 * so an idle sampler sends at once (waiting for an animation frame first
 * added 8 ms on average, measured: notes benchmark-results/pl-picker-loupe-2026-10-31). */
export function createPatchSampler(
  onSample: (sample: PointerSample, inputAt?: number) => void,
  fetch: Fetch = fetchPatch,
  schedule: (run: () => void) => unknown = (run) => run(),
) {
  let pending: Request | null = null;
  let busy = false;
  let scheduled = false;
  let generation = 0;

  const pump = () => {
    scheduled = false;
    if (busy || !pending) return;
    const request = pending;
    pending = null;
    busy = true;
    const asked = generation;
    fetch(request.path, request.x, request.y)
      .catch(() => null)
      .then((patch) => {
        busy = false;
        if (asked === generation)
          onSample({ clientX: request.clientX, clientY: request.clientY, patch }, request.inputAt);
        if (pending) kick();
      });
  };
  const kick = () => {
    if (scheduled) return;
    scheduled = true;
    schedule(pump);
  };
  return {
    request(next: Request) {
      pending = next;
      kick();
    },
    /** Drops what is queued and any answer still on its way. */
    cancel() {
      pending = null;
      generation += 1;
    },
  };
}
