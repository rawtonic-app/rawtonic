import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import { useEditorStore } from '../store/useEditorStore';
import { Invokes } from '../components/ui/AppProperties';
import { Mask, type SubMask } from '../components/panel/right/Masks';
import type { Adjustments, AiPatch, MaskContainer } from '../utils/adjustments';

/**
 * Mask thumbnails in the layer list (Lightroom's Masks panel shows one per
 * mask): white mask on black, in the photo's frame as the viewer shows it,
 * made natively from the already rasterized masks (`generate_mask_thumbnails`)
 * and sent as tiny PNGs. They follow edits once a change settles, never per
 * brush dab or slider step.
 */

/** Thumbnail boxes in CSS pixels (studio.css `.studio-mask-thumb`). */
export const MASK_THUMB_SIZE = {
  layer: [42, 28],
  component: [24, 16],
} as const;

/** Quiet time after the last change before thumbnails are made. */
export const MASK_THUMB_SETTLE_MS = 250;

interface MaskThumbnailState {
  path: string | null;
  /** PNG data URLs by layer id. */
  layers: Record<string, string>;
  /** PNG data URLs by component id. */
  components: Record<string, string>;
}

export const useMaskThumbnailStore = create<MaskThumbnailState>(() => ({
  path: null,
  layers: {},
  components: {},
}));

/** Recipe fields that move or reshape the photo under its masks (crop,
 * orientation, straightening, transform and lens geometry). */
const GEOMETRY_FIELDS = [
  'crop',
  'rotation',
  'flipHorizontal',
  'flipVertical',
  'orientationSteps',
  'transformDistortion',
  'transformVertical',
  'transformHorizontal',
  'transformRotate',
  'transformAspect',
  'transformScale',
  'transformXOffset',
  'transformYOffset',
  'lensDistortionAmount',
  'lensVignetteAmount',
  'lensTcaAmount',
  'lensDistortionParams',
  'lensCorrectionMode',
  'lensMaker',
  'lensModel',
  'lensDistortionEnabled',
  'lensTcaEnabled',
  'autoChromaticAberration',
  'autoCaVersion',
  'lensVignetteEnabled',
  'guidedPerspective',
] as const;

const MASK_DATA_KEYS = new Set(['maskDataBase64', 'mask_data_base64']);

/** Stands in for an AI mask's bitmap in a key: its length and two samples
 * (the PNG's data and checksum), not megabytes of text. */
function summarizeMaskData(key: string, value: unknown) {
  if (MASK_DATA_KEYS.has(key) && typeof value === 'string') {
    const middle = Math.floor(value.length / 2);
    return `${value.length}:${value.slice(middle, middle + 48)}:${value.slice(-48)}`;
  }
  return value;
}

export function geometryKey(adjustments: Partial<Adjustments>): string {
  const record = adjustments as Record<string, unknown>;
  return JSON.stringify(GEOMETRY_FIELDS.map((field) => record[field] ?? null));
}

/** What a layer's thumbnail depends on: its components and invert (not its
 * name, visibility, opacity or adjustments; the thumbnail shows the mask). */
export function layerThumbnailKey(layer: MaskContainer): string {
  return JSON.stringify(
    { invert: !!layer.invert, subMasks: layer.subMasks.map((sub) => ({ ...sub, name: undefined })) },
    summarizeMaskData,
  );
}

/** What a component's thumbnail depends on: its own shape, invert and opacity. */
export function componentThumbnailKey(sub: SubMask): string {
  return JSON.stringify({ ...sub, name: undefined, mode: undefined, visible: undefined }, summarizeMaskData);
}

/** The recipe as the native side needs it, without AI bitmaps it already has
 * (as the preview and overlay requests send it). */
function requestAdjustments(adjustments: Adjustments): Adjustments {
  const { patchesSentToBackend } = useEditorStore.getState();
  const payload = structuredClone(adjustments);
  const strip = (subMasks?: SubMask[]) =>
    subMasks?.forEach((sub) => {
      if (!sub.id || !sub.parameters || !patchesSentToBackend.has(sub.id)) return;
      for (const key of MASK_DATA_KEYS) {
        if (sub.parameters[key] !== undefined) sub.parameters[key] = null;
      }
    });
  payload.masks?.forEach((mask: MaskContainer) => strip(mask.subMasks));
  payload.aiPatches?.forEach((patch: AiPatch) => {
    if (patch.id && patch.patchData && patchesSentToBackend.has(patch.id)) patch.patchData = null;
    strip(patch.subMasks);
  });
  return payload;
}

interface ThumbnailResponse {
  layers: Record<string, string>;
  components: Record<string, string>;
}

/** Whether a mouse button, pen or finger is down anywhere (a brush stroke,
 * a shape or slider drag): thumbnails wait for its release. */
let pointerDown = false;
const onPointerDown = () => {
  pointerDown = true;
};
const onPointerUp = () => {
  pointerDown = false;
};

function deviceScale() {
  return Math.min(2, Math.max(1, Math.ceil(window.devicePixelRatio || 1)));
}

/**
 * Keeps `useMaskThumbnailStore` up to date for the open photo: every layer's
 * thumbnail and the selected layer's components'. Requests only the masks
 * whose shape or the photo's geometry changed, one request at a time, after
 * edits settle (MASK_THUMB_SETTLE_MS without changes and no pointer down).
 */
export function useMaskThumbnailUpdates() {
  useEffect(() => {
    /** Key of each thumbnail last made, by layer or component id. */
    const made = new Map<string, string>();
    let timer: ReturnType<typeof setTimeout> | null = null;
    let busy = false;
    let again = false;
    let failures = 0;
    let disposed = false;

    const schedule = (delay = MASK_THUMB_SETTLE_MS) => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(run, delay);
    };

    const run = async () => {
      timer = null;
      if (disposed) return;
      const editor = useEditorStore.getState();
      if (pointerDown || editor.isSliderDragging) {
        schedule(150);
        return;
      }
      const path = editor.selectedImage?.path ?? null;
      const store = useMaskThumbnailStore.getState();
      if (store.path !== path) {
        made.clear();
        failures = 0;
        useMaskThumbnailStore.setState({ path, layers: {}, components: {} });
      }
      if (!path || !editor.selectedImage?.isReady) return;
      if (busy) {
        again = true;
        return;
      }

      const adjustments = editor.adjustments;
      const geometry = geometryKey(adjustments);
      const masks: MaskContainer[] = adjustments.masks ?? [];
      const selected = masks.find((mask) => mask.id === editor.activeMaskContainerId);
      const keys = new Map<string, string>();
      const layerIds: string[] = [];
      const componentIds: string[] = [];
      for (const mask of masks) {
        const key = geometry + layerThumbnailKey(mask);
        keys.set(mask.id, key);
        if (made.get(mask.id) !== key) layerIds.push(mask.id);
      }
      for (const sub of selected?.subMasks ?? []) {
        if (sub.type === Mask.LumaRange) continue;
        const key = geometry + componentThumbnailKey(sub);
        keys.set(sub.id, key);
        if (made.get(sub.id) !== key) componentIds.push(sub.id);
      }
      // Forget deleted layers and components (kept components of other
      // layers stay for when their layer is selected again).
      const layerSet = new Set(masks.map((mask) => mask.id));
      const subSet = new Set(masks.flatMap((mask) => mask.subMasks.map((sub) => sub.id)));
      const prune = (record: Record<string, string>, alive: Set<string>) =>
        Object.keys(record).some((id) => !alive.has(id))
          ? Object.fromEntries(Object.entries(record).filter(([id]) => alive.has(id)))
          : record;
      const current = useMaskThumbnailStore.getState();
      const prunedLayers = prune(current.layers, layerSet);
      const prunedComponents = prune(current.components, subSet);
      if (prunedLayers !== current.layers || prunedComponents !== current.components) {
        useMaskThumbnailStore.setState({ layers: prunedLayers, components: prunedComponents });
      }
      if (layerIds.length === 0 && componentIds.length === 0) return;

      const scale = deviceScale();
      const size = (box: readonly [number, number]) => [box[0] * scale, box[1] * scale];
      busy = true;
      try {
        const result = await invoke<ThumbnailResponse>(Invokes.GenerateMaskThumbnails, {
          jsAdjustments: requestAdjustments(adjustments),
          layerIds,
          componentIds,
          layerSize: size(MASK_THUMB_SIZE.layer),
          componentSize: size(MASK_THUMB_SIZE.component),
        });
        if (disposed || useEditorStore.getState().selectedImage?.path !== path) return;
        failures = 0;
        const state = useMaskThumbnailStore.getState();
        const layers = { ...state.layers };
        const components = { ...state.components };
        const apply = (ids: string[], into: Record<string, string>, from: Record<string, string> = {}) =>
          ids.forEach((id) => {
            made.set(id, keys.get(id) ?? '');
            if (from[id]) into[id] = from[id];
            else delete into[id];
          });
        apply(layerIds, layers, result?.layers);
        apply(componentIds, components, result?.components);
        useMaskThumbnailStore.setState({ layers, components });
      } catch (error) {
        // The photograph may still be loading: try again a few times.
        failures += 1;
        if (failures <= 5) again = true;
        else console.warn('Mask thumbnails failed:', error);
      } finally {
        busy = false;
        if (again && !disposed) {
          again = false;
          schedule(failures > 0 ? 400 * failures : 0);
        }
      }
    };

    window.addEventListener('pointerdown', onPointerDown, true);
    window.addEventListener('pointerup', onPointerUp, true);
    window.addEventListener('pointercancel', onPointerUp, true);
    window.addEventListener('blur', onPointerUp);
    const unsubscribe = useEditorStore.subscribe((state, prev) => {
      if (
        state.adjustments !== prev.adjustments ||
        state.selectedImage?.path !== prev.selectedImage?.path ||
        state.selectedImage?.isReady !== prev.selectedImage?.isReady ||
        state.activeMaskContainerId !== prev.activeMaskContainerId
      ) {
        // A new photo or layer shows at once; edits wait until they settle.
        const immediate =
          state.selectedImage?.path !== prev.selectedImage?.path ||
          state.activeMaskContainerId !== prev.activeMaskContainerId;
        schedule(immediate ? 0 : MASK_THUMB_SETTLE_MS);
      }
    });
    schedule(0);
    return () => {
      disposed = true;
      unsubscribe();
      if (timer) clearTimeout(timer);
      window.removeEventListener('pointerdown', onPointerDown, true);
      window.removeEventListener('pointerup', onPointerUp, true);
      window.removeEventListener('pointercancel', onPointerUp, true);
      window.removeEventListener('blur', onPointerUp);
      pointerDown = false;
    };
  }, []);
}
