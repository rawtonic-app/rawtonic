import React, { useCallback, useEffect, useLayoutEffect, useRef, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import debounce from 'lodash.debounce';
import { toast } from 'react-toastify';
import throttle from 'lodash.throttle';
import { isRecipeHeld, useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useRelightStore } from '../store/useRelightStore';
import { stripSentRelightData } from '../utils/relight';
import { Adjustments, AiPatch, COPYABLE_ADJUSTMENT_KEYS, MaskContainer, withSharpenVersion } from '../utils/adjustments';
import { Invokes, Panel, canRenderSource } from '../components/ui/AppProperties';
import type { SubMask } from '../components/panel/right/Masks';
import type { TransformWrapperHandle } from '../components/panel/Editor';
import { debouncedSave, exclusiveRecipeOperation, registerScheduledRecipe } from './useEditorActions';
import { saveFailure } from './useLibraryActions';
import { globalImageCache } from '../utils/ImageLRUCache';
import { epochOf, latestPointerDispatchTime, latestPointerInputTime, performanceDiagnosticsEnabled, reportPerformance, notePointerSessionFrame, registerDiagnosticQueue, noteBrowserPreview } from './usePerformanceDiagnostics';
import { LatestPreviewQueue } from '../utils/latestPreviewQueue';
import { sourceDevelopToApply } from '../utils/aiDenoise';
import { originalPreviewAdjustments } from '../utils/beforeAfter';
import { toastAiError } from '../utils/aiErrors';

// Matches PREVIEW_RENDER_FAILED in src-tauri/src/lib.rs.
const PREVIEW_RENDER_FAILED = 'Preview rendering failed';
import {
  LIVE_STROKE_RESYNC,
  forgetLiveStroke,
  liveStrokeFrame,
  registerLiveMaskRenderer,
  type LiveStroke,
  type LiveStrokeArgs,
} from '../utils/liveMaskRender';
import { registerSliderDraftRenderer, wasRenderedAsDraft } from '../utils/sliderDraft';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { frameDrawnMasks } from '../utils/maskGeometry';
import { interactivePreview } from '../utils/interactivePreview';
import { retireDraftPatches, showDraftPatch } from '../utils/browserDraftPatch';
import { previewBuffer } from '../utils/previewBuffer';
import { applyLayerSolo } from '../utils/layerSolo';
import {
  MASK_PARAMETERS_MISSING,
  acknowledgeParameters,
  forgetParameterReferences,
  withParameterReferences,
} from '../utils/maskParameterRefs';
import { useOsPlatform } from './useOsPlatform';

interface PreviewRequest {
  path: string;
  adjustments: Adjustments;
  dragging: boolean;
  targetRes?: number;
  live?: boolean;
  /** Profiling only: the pointer event that produced this live frame. */
  inputAt?: number;
  /** Profiling only: when that event reached the page's capture listener. */
  dispatchAt?: number;
  /** The uncommitted brush stroke a live frame shows. */
  liveStroke?: LiveStroke;
}

// The native worker drops requests older than the newest it has taken. Job ids
// restart when the webview reloads, so they are offset by the page's load time
// in microseconds (fewer than 1,000 jobs per millisecond keeps this monotonic).
const PREVIEW_SEQUENCE_BASE = Date.now() * 1000;

/** The recipe last handed to the renderer, and the photograph it belongs to. */
export interface PreviousAdjustments {
  path: string;
  adjustments: Adjustments;
}

export function useImageProcessing(
  transformWrapperRef: React.RefObject<TransformWrapperHandle | null>,
  prevAdjustmentsRef: React.RefObject<PreviousAdjustments | null>,
  renderRefs: {
    previewJobIdRef: React.RefObject<number>;
    latestRenderedJobIdRef: React.RefObject<number>;
    currentResRef: React.RefObject<number>;
  },
) {
  const { previewJobIdRef, latestRenderedJobIdRef, currentResRef } = renderRefs;
  const os = useOsPlatform();

  const selectedImage = useEditorStore((state) => state.selectedImage);
  const adjustments = useEditorStore((state) => state.adjustments);
  const previewOverride = useEditorStore((state) => state.previewOverride);
  const isWaveformVisible = useEditorStore((state) => state.isWaveformVisible);
  const activeWaveformChannel = useEditorStore((state) => state.activeWaveformChannel);
  const displaySize = useEditorStore((state) => state.displaySize);
  const baseRenderSize = useEditorStore((state) => state.baseRenderSize);
  const originalSize = useEditorStore((state) => state.originalSize);
  const isSliderDragging = useEditorStore((state) => state.isSliderDragging);
  const setEditor = useEditorStore((state) => state.setEditor);

  const activeView = useUIStore((state) => state.activeView);
  const activePanel = useUIStore((state) => state.activePanel);
  const appSettings = useSettingsStore((state) => state.appSettings);
  const multiSelectedPaths = useLibraryStore((state) => state.multiSelectedPaths);

  const uncroppedJobIdRef = useRef(0);
  const latestUncroppedJobIdRef = useRef(0);

  const lastAnalyticsTimeRef = useRef<number>(0);
  const activeWaveformChannelRef = useRef(activeWaveformChannel);
  activeWaveformChannelRef.current = activeWaveformChannel;

  const selectedImagePathRef = useRef<string | null>(null);
  selectedImagePathRef.current = selectedImage?.path ?? null;
  // Live brush frames need the native display: the JPEG fallback would encode
  // and crossfade a whole preview for every pointer move.
  const nativeDisplayRef = useRef(false);

  const calculateROI = useCallback(() => {
    if (!transformWrapperRef.current) return null;
    const state = transformWrapperRef.current.instance.transformState;
    if (!state) return null;

    if (!baseRenderSize) return null;

    const { scale, positionX, positionY } = state;
    const { width: baseW, height: baseH, offsetX, offsetY, containerWidth, containerHeight } = baseRenderSize;

    if (!baseW || !baseH || !containerWidth || !containerHeight) return null;
    if (scale <= 1.01) return null;

    const paddingPixels = 2.0;
    const paddingX = paddingPixels / baseW;
    const paddingY = paddingPixels / baseH;

    const visibleLeft = -positionX / scale;
    const visibleTop = -positionY / scale;
    const visibleRight = visibleLeft + containerWidth / scale;
    const visibleBottom = visibleTop + containerHeight / scale;

    const imgLeft = offsetX;
    const imgTop = offsetY;
    const imgRight = offsetX + baseW;
    const imgBottom = offsetY + baseH;

    const intersectLeft = Math.max(visibleLeft, imgLeft);
    const intersectTop = Math.max(visibleTop, imgTop);
    const intersectRight = Math.min(visibleRight, imgRight);
    const intersectBottom = Math.min(visibleBottom, imgBottom);

    if (intersectLeft >= intersectRight || intersectTop >= intersectBottom) {
      return null;
    }

    const roiX = (intersectLeft - imgLeft) / baseW;
    const roiY = (intersectTop - imgTop) / baseH;
    const roiW = (intersectRight - intersectLeft) / baseW;
    const roiH = (intersectBottom - intersectTop) / baseH;

    const newRoiX = roiX - paddingX;
    const newRoiY = roiY - paddingY;
    const newRoiW = roiW + paddingX * 2;
    const newRoiH = roiH + paddingY * 2;

    const clampedX = Math.max(0, newRoiX);
    const clampedY = Math.max(0, newRoiY);
    const clampedW = Math.min(1 - clampedX, newRoiW);
    const clampedH = Math.min(1 - clampedY, newRoiH);

    if (clampedW > 0.999 && clampedH > 0.999) return null;

    return [clampedX, clampedY, clampedW, clampedH] as [number, number, number, number];
  }, [baseRenderSize, transformWrapperRef]);

  const executeApplyAdjustments = useCallback(
    async (
      currentAdjustments: Adjustments,
      dragging: boolean = false,
      targetRes?: number,
      live = false,
      inputAt = 0,
      dispatchAt = 0,
      liveStroke?: LiveStroke,
    ) => {
      const enteredAt = performanceDiagnosticsEnabled ? performance.now() : 0;
      const currentPath = selectedImage?.path;
      if (!currentPath) return;

      let shouldRequestAnalytics = false;
      if (dragging || live) {
        const now = performance.now();
        if (now - lastAnalyticsTimeRef.current > 100) {
          shouldRequestAnalytics = true;
          lastAnalyticsTimeRef.current = now;
        }
      } else {
        shouldRequestAnalytics = true;
        lastAnalyticsTimeRef.current = 0;
      }

      // While cropping, the native display shows the photo uncropped and
      // unrotated and turns it by the straighten angle itself (crop editing
      // after Capture One and Lightroom: the photo stays on screen). Crop and
      // angle changes therefore re-use the same frame.
      const cropEdit = isNativeCropEditing();
      // A live stroke sends the recipe once, then only its new points.
      let strokeArgs: LiveStrokeArgs | null = null;
      let withRecipe = true;
      if (liveStroke && !cropEdit) {
        ({ args: strokeArgs, withRecipe } = liveStrokeFrame(liveStroke));
      } else {
        forgetLiveStroke();
      }
      // Other requests refer to brush strokes the backend already holds.
      const referenced = live ? null : withParameterReferences(currentAdjustments);
      const payload = withRecipe ? withViewStates(referenced ? referenced.adjustments : currentAdjustments) : null;
      const { patchesSentToBackend } = useEditorStore.getState();
      const newlySentPatches = new Set<string>();
      if (payload) {
        if (cropEdit) {
          payload.crop = null;
          payload.rotation = 0;
        }

        const processSubMasks = (subMasks: SubMask[]) => {
          if (!Array.isArray(subMasks)) return;
          subMasks.forEach((sm: SubMask) => {
            if (sm.id && sm.parameters) {
              const keys = ['mask_data_base64', 'maskDataBase64'];
              let foundMaskData = false;

              for (const key of keys) {
                if (sm.parameters[key] !== undefined && sm.parameters[key] !== null) {
                  foundMaskData = true;
                  if (patchesSentToBackend.has(sm.id)) {
                    sm.parameters[key] = null;
                  }
                }
              }
              if (foundMaskData && !patchesSentToBackend.has(sm.id)) {
                newlySentPatches.add(sm.id);
              }
            }
          });
        };

        if (payload.aiPatches && Array.isArray(payload.aiPatches)) {
          payload.aiPatches.forEach((p: AiPatch) => {
            if (p.id && p.patchData && !p.isLoading) {
              if (patchesSentToBackend.has(p.id)) {
                p.patchData = null;
              } else {
                newlySentPatches.add(p.id);
              }
            }
            if (p.subMasks) processSubMasks(p.subMasks);
          });
        }

        if (payload.masks && Array.isArray(payload.masks)) {
          payload.masks.forEach((container: MaskContainer) => {
            if (container.subMasks) processSubMasks(container.subMasks);
          });
        }
        stripSentRelightData(payload, patchesSentToBackend).forEach((name) => newlySentPatches.add(name));
      }

      const jobId = ++previewJobIdRef.current;
      const roi = cropEdit ? null : calculateROI();
      const performanceStart = performanceDiagnosticsEnabled ? performance.now() : 0;

      try {
        const buffer: ArrayBuffer = await previewBuffer(Invokes.ApplyAdjustments, {
          jsAdjustments: payload,
          liveStroke: strokeArgs,
          isInteractive: dragging,
          targetResolution: targetRes || null,
          roi: roi || null,
          requestAnalytics: shouldRequestAnalytics,
          computeWaveform: !!isWaveformVisible,
          activeWaveformChannel: activeWaveformChannelRef.current || null,
          sequence: PREVIEW_SEQUENCE_BASE + jobId,
          // Drawn by the native display over this frame (mask layers only).
          maskOverlay: cropEdit ? null : useEditorStore.getState().nativeMaskOverlay,
          cropEdit,
          // Large Linux patches go directly to the image loader. Small
          // frames, final previews and other platforms retain binary replies.
          browserPreviewUri: import.meta.env.VITE_RAWTONIC_PREVIEW_URI !== '0',
        }, os === 'linux' && dragging && import.meta.env.VITE_RAWTONIC_PREVIEW_URI !== '0');
        const returnedAt = performanceDiagnosticsEnabled ? performance.now() : 0;
        if (performanceDiagnosticsEnabled) {
          const performanceEnd = performance.now();
          // Wall-clock bounds let the log split IPC from native preview work.
          const stage = live
            ? 'preview_live_roundtrip'
            : dragging
              ? 'preview_interactive_roundtrip'
              : 'preview_final_roundtrip';
          reportPerformance(stage, {
            sequence: PREVIEW_SEQUENCE_BASE + jobId,
            ms: performanceEnd - performanceStart,
            start_epoch_ms: epochOf(performanceStart),
            end_epoch_ms: epochOf(performanceEnd),
            // A live stroke's frame: whether it carried the recipe, and the
            // points it carried otherwise.
            ...(strokeArgs ? { with_recipe: withRecipe ? 1 : 0, stroke_points_sent: strokeArgs.points.length } : {}),
            // Pointer event to the reply: native presentation is queued;
            // browser previews still need JPEG decode (tracked below).
            // Neither measurement includes display scanout.
            ...(inputAt > 0 ? { input_ms: performanceEnd - inputAt } : {}),
            // Event time stamp to its capture listener, listener to this
            // request (canvas handlers, overlay request, queue wait), and the
            // payload preparation before the IPC call.
            ...(inputAt > 0 && dispatchAt > 0
              ? {
                  dispatch_ms: dispatchAt - inputAt,
                  handler_ms: enteredAt - dispatchAt,
                  prep_ms: performanceStart - enteredAt,
                }
              : {}),
          });
          if ((live || dragging) && inputAt > 0) notePointerSessionFrame('preview', performanceEnd - inputAt);
        }

        if (newlySentPatches.size > 0) {
          newlySentPatches.forEach((id) => patchesSentToBackend.add(id));
        }
        if (referenced) acknowledgeParameters(referenced.sent);

        if (currentPath !== selectedImagePathRef.current) return;

        if (buffer && buffer.byteLength > 0 && jobId >= latestRenderedJobIdRef.current) {
          latestRenderedJobIdRef.current = jobId;

          const textDecoder = new TextDecoder();
          const prefix = textDecoder.decode(buffer.slice(0, 11));
          nativeDisplayRef.current = prefix === 'WGPU_RENDER';
          if (prefix === 'WGPU_RENDER') {
            setEditor((state) => {
              if (state.interactivePatch?.url.startsWith('blob:')) URL.revokeObjectURL(state.interactivePatch.url);
              return { interactivePatch: null, nativeCropFrame: cropEdit };
            });
            // A finished frame may have a new framing, resolution or source
            // (the full decode): the split's before follows (unchanged
            // requests cost the backend nothing).
            if (!dragging && !live && !cropEdit) requestSplitBefore(targetRes || currentResRef.current);
            return;
          }

          if (dragging) {
            const patch = interactivePreview(buffer);
            noteBrowserPreview(patch.url, returnedAt, inputAt, jobId);
            if (os === 'linux' && showDraftPatch(patch, jobId)) return;

            setEditor((state) => {
              const previousUrl = state.interactivePatch?.url;
              if (previousUrl?.startsWith('blob:')) setTimeout(() => URL.revokeObjectURL(previousUrl), 100);
              return { interactivePatch: patch };
            });
          } else {
            const blob = new Blob([buffer], { type: 'image/jpeg' });
            const url = URL.createObjectURL(blob);
            noteBrowserPreview(url, returnedAt, inputAt, jobId);
            retireDraftPatches();

            if (currentPath !== selectedImagePathRef.current || jobId < latestRenderedJobIdRef.current) {
              URL.revokeObjectURL(url);
              return;
            }

            setEditor((state) => {
              const prevUrl = state.finalPreviewUrl;
              if (prevUrl && prevUrl.startsWith('blob:') && !globalImageCache.isProtected(prevUrl)) {
                setTimeout(() => {
                  if (!globalImageCache.isProtected(prevUrl)) {
                    URL.revokeObjectURL(prevUrl);
                  }
                }, 250);
              }
              return { finalPreviewUrl: url };
            });

            setEditor((state) => {
              const oldUrl = state.interactivePatch?.url;
              if (oldUrl?.startsWith('blob:')) {
                setTimeout(() => URL.revokeObjectURL(oldUrl), 500);
              }
              return { interactivePatch: null };
            });
          }
        }
      } catch (err) {
        if (referenced && typeof err === 'string' && err.startsWith(MASK_PARAMETERS_MISSING)) {
          // The backend lost strokes it held: send them in full again.
          forgetParameterReferences();
          await executeRef.current(currentAdjustments, dragging, targetRes, live, inputAt, dispatchAt);
          return;
        }
        if (err === LIVE_STROKE_RESYNC) forgetLiveStroke();
        else if (err !== 'Superseded or worker failed') {
          console.error('Failed to apply adjustments:', err);
        }
        // The native worker could not render at all (e.g. an invalid GPU shader).
        // Say so instead of leaving a stale or black frame; one toast at a time.
        if (typeof err === 'string' && err.startsWith(PREVIEW_RENDER_FAILED)) {
          toast.error(err, { toastId: PREVIEW_RENDER_FAILED });
        }
        if (!dragging) {
          setEditor((state) => {
            if (state.interactivePatch?.url.startsWith('blob:')) URL.revokeObjectURL(state.interactivePatch.url);
            return { interactivePatch: null };
          });
        }
      }
    },
    [os, selectedImage?.path, calculateROI, isWaveformVisible, setEditor, previewJobIdRef, latestRenderedJobIdRef, currentResRef],
  );

  const executeRef = useRef(executeApplyAdjustments);
  executeRef.current = executeApplyAdjustments;
  // Requests go to the native worker right away instead of waiting for the
  // previous response (5–7 ms with real mouse drags). The worker keeps one
  // waiting request (src-tauri/src/preview_slot.rs): a newer one replaces and
  // rejects it on arrival, so each render starts from the latest pointer state
  // even when a render outlasts several moves; sequence numbers keep it in
  // submission order. Four in flight bound the backlog if the GPU stalls.
  const previewQueue = useMemo(
    () => new LatestPreviewQueue<PreviewRequest>(async (request) => {
      const image = useEditorStore.getState().selectedImage;
      if (!canRenderSource(image) || image.path !== request.path) return;
      await executeRef.current(
        request.adjustments,
        request.dragging,
        request.targetRes,
        request.live,
        request.inputAt,
        request.dispatchAt,
        request.liveStroke,
      );
    }, 4),
    [],
  );
  useLayoutEffect(() => () => previewQueue.clear(), [previewQueue, selectedImage?.path]);
  useEffect(() => registerDiagnosticQueue('preview', previewQueue), [previewQueue]);
  // Quitting saves the recipe of a slider still being dragged (its save is
  // scheduled only when the drag ends).
  useEffect(() => registerScheduledRecipe(() => prevAdjustmentsRef.current), [prevAdjustmentsRef]);
  useLayoutEffect(() => {
    nativeDisplayRef.current = false;
  }, [selectedImage?.path]);

  // Brush strokes render the developed image at the committed preview
  // resolution while the mask overlay stays on top. The display then redraws
  // only the changed pixels; saving and history wait for the committed stroke.
  useEffect(
    () =>
      registerLiveMaskRenderer((liveAdjustments, liveStroke) => {
        const editor = useEditorStore.getState();
        const image = editor.selectedImage;
        const targetRes = currentResRef.current;
        if (
          !canRenderSource(image) ||
          !nativeDisplayRef.current ||
          !targetRes ||
          editor.isSliderDragging ||
          editor.previewOverride ||
          useSettingsStore.getState().appSettings?.enableLivePreviews === false
        ) {
          return;
        }
        previewQueue.submit({
          path: image.path,
          adjustments: liveAdjustments,
          dragging: false,
          targetRes,
          live: true,
          inputAt: performanceDiagnosticsEnabled ? latestPointerInputTime() : 0,
          dispatchAt: performanceDiagnosticsEnabled ? latestPointerDispatchTime() : 0,
          liveStroke,
        });
      }),
    [previewQueue, currentResRef],
  );

  // View options change the rendered pixels without changing the recipe,
  // including in the browser renderer. A native mask overlay alone only
  // needs a native redraw; the browser already displays its overlay separately.
  useEffect(
    () =>
      useEditorStore.subscribe((state, previous) => {
        const viewChanged =
          state.clippingWarnings !== previous.clippingWarnings ||
          state.softProof !== previous.softProof ||
          state.softProofSpace !== previous.softProofSpace ||
          state.softProofWarning !== previous.softProofWarning ||
          state.colorRangeShow !== previous.colorRangeShow ||
          state.layerSolo !== previous.layerSolo;
        const native = nativeDisplayRef.current;
        if (!viewChanged && (!native || state.nativeMaskOverlay === previous.nativeMaskOverlay)) return;
        const image = state.selectedImage;
        const targetRes = currentResRef.current;
        if (!canRenderSource(image) || !targetRes || state.isSliderDragging) return;
        previewQueue.submit({
          path: image.path,
          adjustments: state.previewOverride ?? state.adjustments,
          dragging: false,
          targetRes,
          live: native,
        });
        // Live frames leave the split's before alone: it shows the view
        // states too (clipping, soft proof, colour range), so it follows here.
        if (viewChanged && native) requestSplitBefore(targetRes);
      }),
    [previewQueue, currentResRef],
  );
  useEffect(
    () =>
      useRelightStore.subscribe((relight, previous) => {
        if (relight.solo === previous.solo) return;
        const state = useEditorStore.getState();
        const image = state.selectedImage;
        const targetRes = currentResRef.current;
        if (!canRenderSource(image) || !targetRes || state.isSliderDragging) return;
        previewQueue.submit({
          path: image.path,
          adjustments: state.previewOverride ?? state.adjustments,
          dragging: false,
          targetRes,
          live: nativeDisplayRef.current,
        });
      }),
    [previewQueue, currentResRef],
  );

  // AI Denoise and the colour noise filter's base live in the decode:
  // develop the RAW again when the recipe's AI Denoise amount or colour noise
  // filter differs from the source's (src/utils/aiDenoise.ts), then render.
  const sourceDevelop = sourceDevelopToApply(selectedImage, adjustments, isSliderDragging);
  const developAiDenoise = sourceDevelop?.aiDenoise ?? null;
  const developAiDenoiseVersion = sourceDevelop?.aiDenoiseVersion;
  const developMultiscale = sourceDevelop?.multiscale;
  // The newest develop request: it alone clears the busy note, on every way
  // it ends (done, failed, cancelled, photograph switched).
  const sourceDevelopRequestRef = useRef(0);
  useEffect(() => {
    const path = useEditorStore.getState().selectedImage?.path;
    if (developAiDenoise === null || !path) return;
    const amount = developAiDenoise;
    const version = developAiDenoiseVersion;
    const multiscale = developMultiscale;
    const current = useEditorStore.getState().selectedImage;
    const changesAiDenoise =
      amount !== Math.round(current?.aiDenoise ?? 0) ||
      (amount > 0 && (version ?? 1) !== (current?.aiDenoiseVersion ?? 1));
    const changesFilter = multiscale !== current?.colorNoiseMultiscale;
    const request = ++sourceDevelopRequestRef.current;
    const settleBusy = () => {
      if (request === sourceDevelopRequestRef.current && useEditorStore.getState().aiDenoiseBusy)
        setEditor({ aiDenoiseBusy: false });
    };
    if (changesAiDenoise) setEditor({ aiDenoiseBusy: true });
    invoke<number>(Invokes.ApplyAiDenoise, { path, amount, version, multiscale })
      .then((applied) => {
        const editor = useEditorStore.getState();
        if (editor.selectedImage?.path !== path) return settleBusy();
        settleBusy();
        editor.setEditor({
          selectedImage: {
            ...editor.selectedImage,
            aiDenoise: Math.round(applied),
            aiDenoiseVersion: applied > 0 ? (version ?? 1) : 1,
            ...(multiscale === undefined ? {} : { colorNoiseMultiscale: multiscale }),
          },
        });
        const targetRes = currentResRef.current;
        if (targetRes) {
          previewQueue.submit({
            path,
            adjustments: editor.previewOverride ?? editor.adjustments,
            dragging: false,
            targetRes,
          });
        }
      })
      .catch((err) => {
        const editor = useEditorStore.getState();
        settleBusy();
        // A newer request or another photograph replaced this one.
        if (String(err).includes('Load cancelled')) return;
        editor.setEditor({
          ...(editor.selectedImage?.path === path
            ? {
                // Not retried: the source stays as it is.
                selectedImage: {
                  ...editor.selectedImage,
                  ...(changesAiDenoise ? { aiDenoiseFailed: amount, aiDenoiseFailedVersion: version ?? 1 } : {}),
                  ...(!changesAiDenoise && changesFilter ? { colorNoiseMultiscaleFailed: multiscale } : {}),
                },
              }
            : {}),
        });
        if (changesAiDenoise) toastAiError(err, `AI Denoise failed: ${err}`);
        else console.error('Developing the source again failed:', err);
      });
  }, [developAiDenoise, developAiDenoiseVersion, developMultiscale, setEditor, previewQueue, currentResRef]);

  const applyAdjustments = useCallback(
    (currentAdjustments: Adjustments, dragging: boolean = false, targetRes?: number) => {
      if (!canRenderSource(selectedImage)) return;
      previewQueue.submit({
        path: selectedImage!.path, adjustments: currentAdjustments, dragging, targetRes,
        inputAt: dragging && performanceDiagnosticsEnabled ? latestPointerInputTime() : 0,
        dispatchAt: dragging && performanceDiagnosticsEnabled ? latestPointerDispatchTime() : 0,
      });
    },
    [selectedImage, previewQueue],
  );

  const throttledUncroppedPreview = useMemo(
    () =>
      throttle(
        (adj: Adjustments) => {
          if (!useEditorStore.getState().selectedImage?.isReady) return;
          const jobId = ++uncroppedJobIdRef.current;
          invoke<string>(Invokes.GenerateUncroppedPreview, { jsAdjustments: adj })
            .then((dataUrl) => {
              if (jobId >= latestUncroppedJobIdRef.current) {
                latestUncroppedJobIdRef.current = jobId;
                useEditorStore.getState().setEditor({ uncroppedAdjustedPreviewUrl: dataUrl });
              }
            })
            .catch(console.error);
        },
        30,
        { leading: true, trailing: true },
      ),
    [],
  );

  const generateUncroppedPreview = useCallback(
    (currentAdjustments: Adjustments) => {
      throttledUncroppedPreview(currentAdjustments);
    },
    [throttledUncroppedPreview],
  );

  useEffect(() => {
    if (activeView === 'editor' && activePanel === Panel.Crop && selectedImage?.isReady && !isNativeCropEditing()) {
      generateUncroppedPreview(adjustments);
    }
  }, [activeView, adjustments, activePanel, selectedImage?.isReady, generateUncroppedPreview]);

  const calculateTargetRes = useCallback(() => {
    const baseTargetRes = appSettings?.editorPreviewResolution || 1920;
    if (!(appSettings?.enableZoomHifi ?? true) || displaySize.width === 0) {
      return baseTargetRes;
    }

    const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
    const sharpnessFactor = 1.25;
    const zoomMultiplier = appSettings?.highResZoomMultiplier || 1.0;
    const effectiveDpr = appSettings?.useFullDpiRendering ? dpr : 1;

    let targetRes = Math.max(displaySize.width, displaySize.height) * effectiveDpr * sharpnessFactor * zoomMultiplier;
    targetRes = Math.max(targetRes, 512);

    if (originalSize && originalSize.width > 0 && originalSize.height > 0) {
      const origMax = Math.max(originalSize.width, originalSize.height);
      targetRes = Math.min(targetRes, origMax);
      if (targetRes >= origMax * 0.8) {
        targetRes = origMax;
      }
    }

    if (originalSize && targetRes !== Math.max(originalSize.width, originalSize.height)) {
      targetRes = Math.ceil(targetRes / 256) * 256;
    }

    return Math.round(targetRes);
  }, [
    appSettings?.enableZoomHifi,
    appSettings?.editorPreviewResolution,
    appSettings?.highResZoomMultiplier,
    appSettings?.useFullDpiRendering,
    displaySize.width,
    displaySize.height,
    originalSize,
  ]);

  // A slider drag's moves between store commits (utils/sliderDraft.ts), as
  // the drag branch of the effect below would render them. Multi view and
  // other views keep committing every move.
  const calculateTargetResRef = useRef(calculateTargetRes);
  calculateTargetResRef.current = calculateTargetRes;
  const activeViewRef = useRef(activeView);
  activeViewRef.current = activeView;
  useEffect(
    () =>
      registerSliderDraftRenderer((path, draftAdjustments) => {
        const image = useEditorStore.getState().selectedImage;
        if (
          image?.path !== path ||
          !canRenderSource(image) ||
          activeViewRef.current !== 'editor' ||
          useMultiViewStore.getState().active ||
          useSettingsStore.getState().appSettings?.enableLivePreviews === false
        ) {
          return false;
        }
        previewQueue.submit({
          path,
          adjustments: draftAdjustments,
          dragging: true,
          targetRes: calculateTargetResRef.current(),
          inputAt: performanceDiagnosticsEnabled ? latestPointerInputTime() : 0,
          dispatchAt: performanceDiagnosticsEnabled ? latestPointerDispatchTime() : 0,
        });
        return true;
      }),
    [previewQueue],
  );


  const requestHiFiZoom = useMemo(
    () =>
      debounce((targetRes: number) => {
        if (targetRes > currentResRef.current) {
          currentResRef.current = targetRes;
          const { adjustments, previewOverride } = useEditorStore.getState();
          const renderAdjustments = previewOverride ?? adjustments;
          applyAdjustments(renderAdjustments, false, targetRes);
        }
      }, 50),
    [applyAdjustments, currentResRef],
  );

  useEffect(() => {
    if (activeView === 'editor' && canRenderSource(selectedImage) && displaySize.width > 0 && !isSliderDragging) {
      let baseRes = calculateTargetRes();
      if (originalSize.width > 0 && originalSize.height > 0) {
        const maxRes = Math.max(originalSize.width, originalSize.height);
        if (baseRes > maxRes) baseRes = maxRes;
      }
      const finalRes = Math.round(baseRes);

      if (finalRes > currentResRef.current) {
        requestHiFiZoom(finalRes);
      }
    }
    return () => {
      requestHiFiZoom.cancel();
    };
  }, [
    activeView,
    displaySize.width,
    displaySize.height,
    calculateTargetRes,
    selectedImage?.isReady,
    selectedImage?.isPreviewReady,
    selectedImage?.isDecoded,
    isSliderDragging,
    requestHiFiZoom,
    originalSize,
    currentResRef,
  ]);

  useLayoutEffect(() => {
    // A batch edit is writing this recipe and reloads it afterwards: what is
    // shown (the reloaded result) becomes the save and sync baseline, also
    // while a before preview or a drag is shown, so the next edit syncs only
    // its own change.
    if (selectedImage && isRecipeHeld(useEditorStore.getState())) {
      prevAdjustmentsRef.current = { path: selectedImage.path, adjustments };
    }
    // A preview proxy renders before the full decode; its arrival
    // (isReady) renders again from the full source.
    if (!canRenderSource(selectedImage)) return;

    const targetRes = calculateTargetRes();
    const renderAdjustments = previewOverride ?? adjustments;

    if (activeView !== 'editor') {
      if (isSliderDragging) return;
    }

    if (isSliderDragging) {
      if (appSettings?.enableLivePreviews !== false && !wasRenderedAsDraft(renderAdjustments)) {
        applyAdjustments(renderAdjustments, true, targetRes);
      }
    } else {
      currentResRef.current = targetRes;

      applyAdjustments(renderAdjustments, false, targetRes);

      if (previewOverride) return;

      const prev = prevAdjustmentsRef.current;

      if (!prev || prev.path !== selectedImage.path) {
        prevAdjustmentsRef.current = { path: selectedImage.path, adjustments };
        return;
      }

      const hasAdjustmentsChanged = prev.adjustments !== adjustments;

      if (hasAdjustmentsChanged) {
        // Held by a batch edit: no save or sync (baseline taken above).
        if (isRecipeHeld(useEditorStore.getState())) return;
        debouncedSave(selectedImage.path, adjustments);

        const otherPaths = multiSelectedPaths.filter((p) => p !== selectedImage.path);
        if (appSettings?.copyPasteSettings?.autoSync && otherPaths.length > 0) {
          const delta: Partial<Adjustments> = {};
          const includedKeys = appSettings?.copyPasteSettings?.includedAdjustments || COPYABLE_ADJUSTMENT_KEYS;
          for (const key of Object.keys(adjustments) as Array<keyof Adjustments>) {
            if (includedKeys.includes(key as string)) {
              if (JSON.stringify(adjustments[key]) !== JSON.stringify(prev.adjustments[key])) {
                delta[key] = adjustments[key];
              }
            }
          }
          // Synced drawn masks keep the frame of this photo, so the others
          // place them on the same part of themselves.
          if (delta.masks)
            delta.masks = frameDrawnMasks(delta.masks, adjustments, selectedImage.width, selectedImage.height);
          if (Object.keys(delta).length > 0) {
            otherPaths.forEach((p) => globalImageCache.delete(p));
            // In the other photographs' recipe queue, in the order made (a
            // batch edit asked for after it runs after it).
            exclusiveRecipeOperation(otherPaths, () =>
              invoke(Invokes.ApplyAdjustmentsToPaths, {
                paths: otherPaths,
                adjustments: withSharpenVersion(delta, adjustments),
              }),
            ).catch((err) => {
              console.error('Failed to apply adjustments to multi-selection:', err);
              toast.error(`Failed to sync adjustments to the selection: ${saveFailure(err, otherPaths).message}`);
            });
          }
        }

        prevAdjustmentsRef.current = { path: selectedImage.path, adjustments };
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- runs when the recipe or view changes; applyAdjustments and the resolution refs are read at that moment, so a new applyAdjustments alone must not re-render
  }, [
    activeView,
    adjustments,
    previewOverride,
    selectedImage?.path,
    selectedImage?.isReady,
    selectedImage?.isPreviewReady,
    selectedImage?.isDecoded,
    isSliderDragging,
    multiSelectedPaths,
    appSettings?.enableLivePreviews,
    appSettings?.copyPasteSettings?.includedAdjustments,
    appSettings?.copyPasteSettings?.autoSync,
    isWaveformVisible,
  ]);

  // Turning the split on renders its before beside the frame on screen.
  const splitView = useEditorStore((state) => state.splitView);
  useEffect(() => {
    if (!splitView || !nativeDisplayRef.current) return;
    requestSplitBefore(currentResRef.current || calculateTargetRes());
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only turning the split on triggers; the resolution is read now
  }, [splitView]);

  // Entering or leaving crop changes which frame the display needs.
  const isCropping = activeView === 'editor' && activePanel === Panel.Crop;
  const wasCroppingRef = useRef(isCropping);
  useEffect(() => {
    if (wasCroppingRef.current === isCropping) return;
    wasCroppingRef.current = isCropping;
    const state = useEditorStore.getState();
    if (!canRenderSource(state.selectedImage)) return;
    applyAdjustments(state.previewOverride ?? state.adjustments, false, currentResRef.current || calculateTargetRes());
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only the crop state change triggers; the rest is read now
  }, [isCropping]);

  return {
    applyAdjustments,
    executeApplyAdjustments,
  };
}

/** The recipe as rendered: view states the renderer shows but the recipe never stores. */
export function withViewStates(adjustments: Adjustments): Adjustments {
  const payload = structuredClone(adjustments);
  const view = useEditorStore.getState();
  if (view.clippingWarnings) payload.showClipping = true;
  if (view.softProof) {
    payload.softProof = view.softProofSpace;
    payload.softProofWarning = view.softProofWarning;
  }
  payload.colorRangeShow = view.colorRangeShow;
  // Layer solo shows one layer (or component) without changing the recipe.
  if (view.layerSolo && Array.isArray(payload.masks)) payload.masks = applyLayerSolo(payload.masks, view.layerSolo);
  // Relight's solo view shows one light without changing the recipe.
  const solo = useRelightStore.getState().solo;
  if (solo && payload.relightLights?.some((l) => l.id === solo)) {
    payload.relightLights = payload.relightLights.map((l) => (l.id === solo ? l : { ...l, enabled: false }));
  }
  return payload;
}

let splitBeforeInFlight = false;
let splitBeforeQueued: number | null = null;

/**
 * Asks the native display for the split view's before side (the unedited
 * recipe with the edit's framing) at `targetRes`. One request at a time;
 * while one runs, only the newest further request is kept.
 */
export function requestSplitBefore(targetRes: number) {
  if (splitBeforeInFlight) {
    splitBeforeQueued = targetRes;
    return;
  }
  const editor = useEditorStore.getState();
  const image = editor.selectedImage;
  if (
    !editor.splitView ||
    !canRenderSource(image) ||
    useSettingsStore.getState().appSettings?.useWgpuRenderer === false ||
    isNativeCropEditing()
  ) {
    return;
  }
  splitBeforeInFlight = true;
  invoke<boolean>(Invokes.RenderSplitBefore, {
    path: image.path,
    jsAdjustments: withViewStates(originalPreviewAdjustments(editor.adjustments)),
    targetResolution: targetRes || null,
  })
    .catch((err) => {
      if (typeof err === 'string' && err.startsWith(PREVIEW_RENDER_FAILED)) {
        toast.error(err, { toastId: PREVIEW_RENDER_FAILED });
      } else {
        console.warn('Split view before failed:', err);
      }
    })
    .finally(() => {
      splitBeforeInFlight = false;
      const queued = splitBeforeQueued;
      splitBeforeQueued = null;
      if (queued !== null) requestSplitBefore(queued);
    });
}

/** Whether preview frames are the native display's crop-editing frames. */
export function isNativeCropEditing() {
  const ui = useUIStore.getState();
  const editor = useEditorStore.getState();
  return (
    ui.activeView === 'editor' &&
    ui.activePanel === Panel.Crop &&
    useSettingsStore.getState().appSettings?.useWgpuRenderer !== false &&
    editor.hasRenderedFirstFrame
  );
}
