import { useEffect } from 'react';
import { loadErrorText } from '../utils/loadError';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'react-toastify';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes, type ImageMetadata, type LoadImageResult } from '../components/ui/AppProperties';
import {
  FRINGE_VERSION,
  CAMERA_CA_VERSION,
  AUTO_CA_VERSION,
  initialAdjustmentsFor,
  normalizeLoadedAdjustments,
  type Adjustments,
} from '../utils/adjustments';
import { waitForPendingMetadataSave } from './useEditorActions';
import { RESPONSE_KEYS } from '../utils/settingsClipboard';
import { globalImageCache, type ImageCacheEntry } from '../utils/ImageLRUCache';
import { performanceDiagnosticsEnabled, reportPerformance } from './usePerformanceDiagnostics';

function canShowCameraPreview(recipe: Partial<Adjustments> | null | undefined) {
  if (!recipe) return true;
  // A camera JPEG cannot represent a user's crop or geometry. Keep waiting for
  // that photograph's rendered thumbnail/final preview instead of showing an
  // incorrectly rotated/cropped intermediate frame.
  if (recipe.crop || recipe.flipHorizontal || recipe.flipVertical || recipe.guidedPerspective?.enabled ||
      recipe.lensModel || recipe.lensDistortionParams) return false;
  const geometry = ['orientationSteps', 'rotation', 'transformDistortion', 'transformVertical',
    'transformHorizontal', 'transformRotate', 'transformAspect', 'transformXOffset', 'transformYOffset'] as const;
  return geometry.every((key) => !recipe[key]) && (recipe.transformScale ?? 100) === 100;
}

/** Event of the full decode's draft (image_loader.rs `DRAFT_EVENT`). */
export const DEVELOP_DRAFT_EVENT = 'develop-draft-ready';

type ProxyInfo = { width: number; height: number; isRaw: boolean };

/**
 * Listens for the draft the full decode of `path` hands over before it
 * demosaics (a never-opened photograph has no stored proxy). Resolves once
 * listening, so the decode can start; `draft` resolves with the draft's
 * full size, or never when there is none.
 */
async function listenForDraft(path: string): Promise<{ draft: Promise<ProxyInfo>; stop: () => void }> {
  let found: (info: ProxyInfo) => void = () => {};
  const draft = new Promise<ProxyInfo>((resolve) => {
    found = resolve;
  });
  try {
    const stop = await listen<ProxyInfo & { path: string }>(DEVELOP_DRAFT_EVENT, ({ payload }) => {
      if (payload.path === path) found({ width: payload.width, height: payload.height, isRaw: payload.isRaw });
    });
    return { draft, stop };
  } catch {
    // Without the native side (tests) there is no draft.
    return { draft, stop: () => {} };
  }
}

/** Longest wait of the full decode for a stored proxy's first frame. */
export const PROXY_FRAME_FIRST_MS = 400;

/** Resolves once the open photograph's first frame is shown, or after `ms`. */
export function waitForFirstFrame(ms: number): Promise<void> {
  return new Promise((resolve) => {
    if (useEditorStore.getState().hasRenderedFirstFrame) {
      resolve();
      return;
    }
    let unsubscribe = () => {};
    const timer = setTimeout(() => done(), ms);
    function done() {
      clearTimeout(timer);
      unsubscribe();
      resolve();
    }
    unsubscribe = useEditorStore.subscribe((state) => {
      if (state.hasRenderedFirstFrame) done();
    });
  });
}

export function useImageLoader(cachedEditStateRef: React.RefObject<ImageCacheEntry | null>) {
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const adjustments = useEditorStore((s) => s.adjustments);
  const histogram = useEditorStore((s) => s.histogram);
  const waveform = useEditorStore((s) => s.waveform);
  const finalPreviewUrl = useEditorStore((s) => s.finalPreviewUrl);
  const uncroppedAdjustedPreviewUrl = useEditorStore((s) => s.uncroppedAdjustedPreviewUrl);
  const originalSize = useEditorStore((s) => s.originalSize);
  const previewSize = useEditorStore((s) => s.previewSize);
  const hasRenderedFirstFrame = useEditorStore((s) => s.hasRenderedFirstFrame);

  const setEditor = useEditorStore((s) => s.setEditor);
  const setLibrary = useLibraryStore((s) => s.setLibrary);
  const appSettings = useSettingsStore((s) => s.appSettings);

  const isWgpuActive = appSettings?.useWgpuRenderer !== false && selectedImage?.isReady && hasRenderedFirstFrame;

  useEffect(() => {
    const url = selectedImage?.thumbnailSource === 'embedded' ? selectedImage.thumbnailUrl : undefined;
    return () => {
      if (url?.startsWith('blob:') && useEditorStore.getState().selectedImage?.thumbnailUrl !== url &&
          !globalImageCache.isProtected(url)) URL.revokeObjectURL(url);
    };
  }, [selectedImage?.thumbnailUrl, selectedImage?.thumbnailSource]);

  useEffect(() => {
    if (selectedImage && !selectedImage.isDecoded && selectedImage.path) {
      let isEffectActive = true;
      const stillCurrent = () => isEffectActive && useEditorStore.getState().selectedImage?.path === selectedImage.path;

      const loadAll = async () => {
        try {
          useEditorStore.getState().patchesSentToBackend.clear();
          // Native load_image already clears source-dependent caches. Metadata
          // loading does not depend on decoding, so do both in the same round.
          const readMetadata = () => invoke<ImageMetadata>(Invokes.LoadMetadata, { path: selectedImage.path });
          const pendingSave = waitForPendingMetadataSave(selectedImage.path);
          let unsavedAdjustments: Adjustments | undefined;
          const metadataRequest = (pendingSave
            ? pendingSave.then((unsaved) => {
                unsavedAdjustments = unsaved;
                return readMetadata();
              })
            : readMetadata()).catch((err) => {
              console.error('Failed to load metadata:', err);
              return null;
            });
          // A stored preview proxy answers within milliseconds; only without
          // one the camera JPEG (a large transfer that would delay the other
          // answers) is asked for, and the full decode hands over a draft
          // before it demosaics.
          const storedProxy = invoke<ProxyInfo | null>(Invokes.LoadImageProxy, {
            path: selectedImage.path,
          }).catch(() => null);
          const draftListener = storedProxy.then((proxy) => (proxy ? null : listenForDraft(selectedImage.path)));
          const showCameraPreview = () => {
            // Selected-source previews bypass the background thumbnail
            // queue. Binary JPEG pixels keep their original camera encoding and
            // EXIF orientation; readiness still belongs to the full RAW decode.
            void Promise.all([
              invoke<ArrayBuffer>(Invokes.LoadEmbeddedPreview, { path: selectedImage.path }),
              metadataRequest,
            ]).then(([bytes, metadata]: [ArrayBuffer, ImageMetadata | null]) => {
              if (!isEffectActive || !metadata || !bytes.byteLength ||
                  !canShowCameraPreview(unsavedAdjustments ?? metadata?.adjustments)) return;
              const current = useEditorStore.getState();
              if (current.selectedImage?.path !== selectedImage.path || current.selectedImage.isReady ||
                  current.selectedImage.thumbnailUrl || current.hasRenderedFirstFrame || current.finalPreviewUrl) return;
              const url = URL.createObjectURL(new Blob([bytes], { type: 'image/jpeg' }));
              setEditor({ selectedImage: { ...current.selectedImage, thumbnailUrl: url, thumbnailSource: 'embedded' } });
            }).catch(() => {
              // Optional preview extraction must never fail the full image load.
            });
          };
          if (!selectedImage.thumbnailUrl && !useEditorStore.getState().finalPreviewUrl) {
            void storedProxy.then((proxy) => {
              if (!proxy && isEffectActive) showCameraPreview();
            });
          }
          // The camera's lens data, needed when the recipe has no lens mode;
          // read beside the decode instead of after it.
          const cameraRequest = invoke<{ params: Adjustments['lensDistortionParams'] } | null>(
            Invokes.GetCameraLensProfile, { path: selectedImage.path },
          ).catch(() => null);
          let storedLacks: string[] = [];
          // The recipe to render: saved, or the defaults for a never-edited photo.
          const recipeFor = async (metadata: ImageMetadata | null, isRaw: boolean, width: number, height: number) => {
            const savedAdjustments = unsavedAdjustments ?? metadata?.adjustments;
            // Versions a saved recipe lacks are resolved here, not in it.
            storedLacks =
              savedAdjustments && !savedAdjustments.is_null && !unsavedAdjustments
                ? RESPONSE_KEYS.filter((key) => (savedAdjustments as Record<string, unknown>)[key] == null)
                : [];
            const initialAdjusts = savedAdjustments && !savedAdjustments.is_null
              ? normalizeLoadedAdjustments(savedAdjustments, isRaw)
              : initialAdjustmentsFor(isRaw);
            if (!initialAdjusts.aspectRatio && !initialAdjusts.crop) {
              initialAdjusts.aspectRatio = width / height;
            }
            // Profile corrections are on by default: a never-edited RAW (its
            // recipe has no lens mode at all) starts with the camera's own lens
            // data, as its export does (embedded_lens.rs).
            const neverEdited = !savedAdjustments || savedAdjustments.is_null
              || (savedAdjustments as Partial<Adjustments>).lensCorrectionMode === undefined;
            if (neverEdited && isRaw) {
              // And with the edge-fringe pass, as its export (edge_fringe.rs).
              initialAdjusts.fringeVersion = FRINGE_VERSION;
              // And with the automatic lateral CA (lateral_ca.rs).
              initialAdjusts.autoCaVersion = AUTO_CA_VERSION;
              const camera = await cameraRequest;
              if (camera?.params) {
                initialAdjusts.lensCorrectionMode = 'camera';
                initialAdjusts.lensDistortionParams = camera.params;
                initialAdjusts.cameraCaVersion = CAMERA_CA_VERSION;
              }
            }
            return initialAdjusts;
          };
          const publishSource = (
            initialAdjusts: Adjustments,
            width: number,
            height: number,
            image: Partial<NonNullable<typeof selectedImage>>,
          ) => {
            const maxSize = appSettings?.editorPreviewResolution;
            const previewScale = maxSize ? Math.min(1, maxSize / Math.max(width, height)) : 0;
            // Readiness is published together with the native source's
            // dimensions and current recipe. Cached UI pixels may be shown during
            // loading, but they must never make a new recipe render against the
            // old source.
            setEditor((state) => {
              if (state.selectedImage?.path !== selectedImage.path) return state;
              return {
                adjustments: initialAdjusts,
                storedLacksResponses: storedLacks,
                history: [initialAdjusts],
                historyIndex: 0,
                panelsHeld: false,
                originalSize: { width, height },
                previewSize: {
                  width: Math.round(width * previewScale),
                  height: Math.round(height * previewScale),
                },
                selectedImage: { ...state.selectedImage, ...image, height, width },
              };
            });
          };
          // A preview proxy from the session cache renders the developed photo
          // at once (Capture One's preview files); the full decode follows.
          // Set by the proxy render (a callback, so not narrowed by TypeScript).
          const published: { proxy: { width: number; height: number } | null } = { proxy: null };
          const loadStarted = performance.now();
          const mark = (stage: string) =>
            performanceDiagnosticsEnabled &&
            reportPerformance(stage, { ms: Math.round(performance.now() - loadStarted) });
          mark('loader_start');
          // Settles when the decode is in (or failed): a draft still awaited
          // then no longer matters.
          let decodeSettled: () => void = () => {};
          const decodeDone = new Promise<null>((resolve) => {
            decodeSettled = () => resolve(null);
          });
          const proxyRender = Promise.all([
            storedProxy
              .then(async (proxy) => {
                if (proxy) return proxy;
                const listener = await draftListener;
                return listener ? Promise.race([listener.draft, decodeDone]) : null;
              })
              .catch(() => null)
              .finally(() => mark('loader_proxy_answer')),
            metadataRequest.finally(() => mark('loader_metadata_answer')),
          ]).then(async ([proxy, metadata]) => {
            if (!proxy || !stillCurrent() || useEditorStore.getState().selectedImage?.isDecoded) return;
            const recipe = await recipeFor(metadata, proxy.isRaw, proxy.width, proxy.height);
            mark('loader_proxy_recipe');
            if (!stillCurrent() || useEditorStore.getState().selectedImage?.isDecoded) return;
            published.proxy = { width: proxy.width, height: proxy.height };
            // Editable at once; the decode then only sharpens the frame.
            publishSource(recipe, proxy.width, proxy.height, {
              isRaw: proxy.isRaw,
              isPreviewReady: true,
              isReady: true,
              metadata,
            });
          });
          // With a stored proxy its developed frame comes first: the full
          // decode, started at the same time, takes the cores the proxy frame
          // needs (on a busy M5 Pro a 46 MP NEF's first frame came at 183 ms
          // instead of 65 ms, a 40 MP RAF's at 475 instead of 105 ms; on an
          // idle one the two orders were within ~50 ms). Without a stored
          // proxy the decode starts at once.
          // Without one it starts as soon as the draft can be received.
          const decodeStart = storedProxy.then((proxy) =>
            proxy ? waitForFirstFrame(PROXY_FRAME_FIRST_MS) : draftListener.then(() => undefined),
          );
          const decode = decodeStart
            .then(() => {
              // A photograph left while its decode waited is not decoded: the
              // backend would cancel the next photograph's decode for it.
              if (!stillCurrent()) throw new Error('Photograph left before its decode started');
              return invoke<LoadImageResult>(Invokes.LoadImage, { path: selectedImage.path });
            })
            .finally(() => {
              decodeSettled();
              void draftListener.then((listener) => listener?.stop());
            });
          const [metadata, loadImageResult]: [ImageMetadata | null, LoadImageResult] = await Promise.all([
            metadataRequest,
            decode,
          ]);
          if (!stillCurrent()) return;
          // The decode never waits for the proxy: a proxy still on its way
          // gives up once the source is ready.
          void proxyRender;

          const { width, height } = loadImageResult;
          const storedMetadata = metadata ?? loadImageResult.metadata;
          const fullSource = {
            exif: loadImageResult.exif,
            isRaw: loadImageResult.is_raw,
            isReady: true,
            isDecoded: true,
            aiDenoise: loadImageResult.ai_denoise ?? 0,
            aiDenoiseVersion: loadImageResult.ai_denoise_version ?? 1,
            colorNoiseMultiscale: loadImageResult.color_noise_multiscale ?? undefined,
            // Offline: the preview proxy is all there is (no decode follows).
            offline: loadImageResult.offline ?? null,
            ...(loadImageResult.ai_denoise_error
              ? {
                  aiDenoiseFailed: Number(storedMetadata?.adjustments?.aiDenoise ?? 0),
                  aiDenoiseFailedVersion: Number(storedMetadata?.adjustments?.aiDenoiseVersion ?? 1),
                }
              : {}),
            metadata: storedMetadata,
          };
          if (loadImageResult.ai_denoise_error) {
            toast.error(`AI Denoise is not available: ${loadImageResult.ai_denoise_error}`);
          }
          if (published.proxy) {
            // The recipe and history stay, with any edits made on the proxy;
            // only the source becomes the decode. A fast draft decode may
            // round the size differently: the default aspect then follows.
            const draft = published.proxy;
            setEditor((state) => {
              if (state.selectedImage?.path !== selectedImage.path) return state;
              const adjustments = state.adjustments;
              const sizeChanged = draft.width !== width || draft.height !== height;
              const followAspect = sizeChanged && !adjustments.crop &&
                Math.abs((adjustments.aspectRatio ?? 0) - draft.width / draft.height) < 1e-9;
              return {
                ...(followAspect ? { adjustments: { ...adjustments, aspectRatio: width / height } } : {}),
                originalSize: { width, height },
                selectedImage: { ...state.selectedImage, ...fullSource, height, width },
              };
            });
          } else {
            const initialAdjusts = await recipeFor(storedMetadata, loadImageResult.is_raw, width, height);
            if (!stillCurrent()) return;
            publishSource(initialAdjusts, width, height, fullSource);
          }
        } catch (err) {
          if (stillCurrent()) {
            console.error('Failed to load image:', err);
            // Selecting an offline photograph in the Library prepares Develop
            // in the background; without a preview that cannot work, which
            // its Offline badge already says. Develop itself reports it.
            const offline = useLibraryStore
              .getState()
              .imageList.some((image) => image.path === selectedImage.path && image.is_offline);
            if (!offline || useUIStore.getState().activeView === 'editor') {
              toast.error(loadErrorText(err, selectedImage.path));
            }
            setEditor({ selectedImage: null, panelsHeld: false });
          }
        } finally {
          if (stillCurrent()) {
            setLibrary({ isViewLoading: false });
          }
        }
      };

      loadAll();

      return () => {
        isEffectActive = false;
      };
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed on path and isReady; this load itself rewrites selectedImage (embedded preview, readiness) and checks the current image via getState(), so other changes must not restart it
  }, [
    selectedImage?.path,
    selectedImage?.isDecoded,
    appSettings?.editorPreviewResolution,
    setEditor,
    setLibrary,
  ]);

  useEffect(() => {
    if (selectedImage?.path && selectedImage.isReady && (finalPreviewUrl || isWgpuActive)) {
      cachedEditStateRef.current = {
        adjustments,
        histogram,
        waveform,
        finalPreviewUrl,
        uncroppedPreviewUrl: uncroppedAdjustedPreviewUrl,
        selectedImage,
        originalSize,
        previewSize,
      };
    } else {
      cachedEditStateRef.current = null;
    }
  }, [
    selectedImage,
    adjustments,
    histogram,
    waveform,
    finalPreviewUrl,
    uncroppedAdjustedPreviewUrl,
    originalSize,
    previewSize,
    isWgpuActive,
    cachedEditStateRef,
  ]);
}
