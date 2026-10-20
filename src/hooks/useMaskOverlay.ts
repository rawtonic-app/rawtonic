import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useEditorStore } from '../store/useEditorStore';
import { Invokes } from '../components/ui/AppProperties';
import type { SubMask } from '../components/panel/right/Masks';
import type { Adjustments, AiPatch, MaskAdjustments, MaskContainer } from '../utils/adjustments';
import { LatestPreviewQueue } from '../utils/latestPreviewQueue';
import { epochOf, forgetMaskOverlayLoad, notePointerSessionFrame, performanceDiagnosticsEnabled, registerDiagnosticQueue, reportPerformance, trackMaskOverlayLoad } from './usePerformanceDiagnostics';

/** The mask layer drawn by `generate_mask_overlay`, which reads it as
 * `MaskDefinition` (mask_generation.rs). This hook reads only `visible` and `subMasks`. */
export interface OverlayMaskDefinition {
  visible: boolean;
  subMasks: SubMask[];
  /** The overlay ignores them. */
  adjustments?: MaskAdjustments | Record<string, never>;
  opacity?: number;
}

interface OverlayRequest {
  maskDef: OverlayMaskDefinition | null;
  renderSize: { width: number; height: number; scale: number };
  adjustments: Adjustments;
  path: string;
  selection: (string | null)[];
  generation: number;
  requestedAt: number;
  style: MaskOverlayStyle;
}

/** Capture One's grayscale view, or the overlay colour (RGB) and opacity (%). */
export interface MaskOverlayStyle {
  grayscale?: boolean;
  color?: [number, number, number];
  opacity?: number;
}

/** Drops the bitmaps the backend already holds (it hydrates them by id). */
function stripSentMaskData(subMasks: SubMask[]) {
  if (!Array.isArray(subMasks)) return;
  const { patchesSentToBackend } = useEditorStore.getState();
  for (const sm of subMasks) {
    if (!sm.id || !sm.parameters || !patchesSentToBackend.has(sm.id)) continue;
    if (sm.parameters.mask_data_base64 !== undefined) sm.parameters.mask_data_base64 = null;
    if (sm.parameters.maskDataBase64 !== undefined) sm.parameters.maskDataBase64 = null;
  }
}

/** A copy of the adjustments for the mask commands (`generate_mask_overlay`,
 * `sample_luma_range`), without the bitmaps the backend already holds, so
 * both use the same cached mask source. */
export function adjustmentsForMaskCommand(adjustments: Adjustments): Adjustments {
  const stripped = structuredClone(adjustments);
  stripped.masks?.forEach((mask: MaskContainer) => stripSentMaskData(mask.subMasks));
  stripped.aiPatches?.forEach((patch: AiPatch) => stripSentMaskData(patch.subMasks));
  return stripped;
}

/** Keep one in-flight overlay and the latest pointer state, with binary image
 * delivery and explicit ownership of the temporary image URL. */
export function useMaskOverlay() {
  const path = useEditorStore((state) => state.selectedImage?.path);
  const maskId = useEditorStore((state) => state.activeMaskContainerId);
  const subMaskId = useEditorStore((state) => state.activeMaskId);
  const [maskOverlayUrl, setMaskOverlayUrl] = useState<string | null>(null);
  const generation = useRef(0);
  const mounted = useRef(false);
  const ownedUrl = useRef<string | null>(null);
  const publishOverlay = useCallback((url: string | null) => {
    // Two completed jobs can be batched into one React commit. Own/release each
    // URL synchronously so an intermediate URL cannot escape effect cleanup.
    if (ownedUrl.current && ownedUrl.current !== url) {
      forgetMaskOverlayLoad(ownedUrl.current);
      URL.revokeObjectURL(ownedUrl.current);
    }
    ownedUrl.current = url;
    setMaskOverlayUrl(url);
  }, []);

  const queue = useMemo(() => new LatestPreviewQueue<OverlayRequest>(async (request) => {
    const valid = () => {
      const current = useEditorStore.getState();
      const selection = [current.activeMaskContainerId, current.activeMaskId];
      return mounted.current && generation.current === request.generation &&
        current.selectedImage?.path === request.path &&
        selection.every((id, index) => id === request.selection[index]);
    };
    if (!valid()) return;
    const { maskDef, renderSize, adjustments } = request;
    if (!maskDef?.visible || renderSize.width <= 0 || renderSize.height <= 0) {
      publishOverlay(null);
      return;
    }
    const started = performanceDiagnosticsEnabled ? performance.now() : 0;
    const strippedAdjustments = adjustmentsForMaskCommand(adjustments);
    const strippedMaskDef = structuredClone(maskDef);
    stripSentMaskData(strippedMaskDef.subMasks);
    const invokedAt = performanceDiagnosticsEnabled ? performance.now() : 0;
    try {
      const bytes = await invoke<ArrayBuffer>(Invokes.GenerateMaskOverlay, {
        cropOffset: [adjustments.crop?.x || 0, adjustments.crop?.y || 0],
        height: Math.round(renderSize.height),
        width: Math.round(renderSize.width),
        scale: renderSize.scale,
        maskDef: strippedMaskDef,
        jsAdjustments: strippedAdjustments,
        grayscale: !!request.style.grayscale,
        color: request.style.color,
        opacity: request.style.opacity,
      });
      if (!valid()) return;
      const url = bytes.byteLength ? URL.createObjectURL(new Blob([bytes], { type: 'image/png' })) : null;
      if (url) trackMaskOverlayLoad(url, request.requestedAt);
      publishOverlay(url);
      reportPerformance('mask_overlay_roundtrip', {
        ms: performanceDiagnosticsEnabled ? performance.now() - started : 0,
        prep_ms: invokedAt - started,
        start_epoch_ms: epochOf(invokedAt),
        input_ms: performanceDiagnosticsEnabled ? performance.now() - request.requestedAt : 0,
        width: Math.round(renderSize.width), height: Math.round(renderSize.height),
        layers: adjustments.masks?.length ?? 0,
      });
      if (performanceDiagnosticsEnabled) notePointerSessionFrame('overlay', performance.now() - request.requestedAt);
    } catch (error) {
      if (valid()) publishOverlay(null);
      console.error('Failed to generate live mask overlay:', error);
    }
  }), [publishOverlay]);

  useEffect(() => registerDiagnosticQueue('overlay', queue), [queue]);

  useLayoutEffect(() => {
    mounted.current = true;
    generation.current++;
    queue.clear();
    publishOverlay(null);
    return () => {
      mounted.current = false;
      // eslint-disable-next-line react-hooks/exhaustive-deps -- generation is a request counter, not a DOM ref; cleanup must bump its latest value so in-flight overlay jobs fail valid()
      generation.current++;
      queue.clear();
      if (ownedUrl.current) {
        forgetMaskOverlayLoad(ownedUrl.current);
        URL.revokeObjectURL(ownedUrl.current);
      }
      ownedUrl.current = null;
    };
  }, [path, maskId, subMaskId, queue, publishOverlay]);

  const requestMaskOverlay = useCallback((
    maskDef: OverlayMaskDefinition | null,
    renderSize: OverlayRequest['renderSize'],
    adjustments: Adjustments,
    style: MaskOverlayStyle = {},
  ) => {
    if (!maskDef?.visible || renderSize.width <= 0 || renderSize.height <= 0) {
      generation.current++;
      queue.clear();
      publishOverlay(null);
      return;
    }
    const current = useEditorStore.getState();
    const currentPath = current.selectedImage?.path;
    if (!currentPath) return;
    queue.submit({ maskDef, renderSize, adjustments, path: currentPath,
      selection: [current.activeMaskContainerId, current.activeMaskId],
      generation: generation.current,
      requestedAt: performanceDiagnosticsEnabled ? performance.now() : 0,
      style });
    // Changing the callback also asks Editor's overlay effect to regenerate the
    // composite after selecting another child within the same container.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- path and the selection ids are listed only to change the callback's identity (see above); the body reads them via getState()
  }, [queue, publishOverlay, path, maskId, subMaskId]);

  return { maskOverlayUrl, requestMaskOverlay };
}
