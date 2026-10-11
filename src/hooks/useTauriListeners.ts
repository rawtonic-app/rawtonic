import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { CullingSuggestions, ImageFile, Invokes, Progress } from '../components/ui/AppProperties';
import { Status } from '../components/ui/ExportImportProperties';
import { ExportJob, ExternalEditSession, useProcessStore } from '../store/useProcessStore';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { libraryImageIndex } from '../utils/libraryIndex';
import { setMergeStateFor } from './useProductivityActions';
import { confirmQuitWithUnsaved, handleQuitRequest } from '../utils/quit';
import { recountAfterFileOperation } from '../utils/offline';

type EditorState = ReturnType<typeof useEditorStore.getState>;

// Event payloads, as emitted by the backend in src-tauri/src.
interface AnalyticsUpdatePayload {
  path: string;
  // image_processing::HistogramData / WaveformData, typed as the editor store fields they are written to.
  histogram: EditorState['histogram'];
  waveform: EditorState['waveform'];
}

interface ThumbnailGeneratedPayload {
  path: string;
  thumbnailPath: string;
  previewPath: string;
  rating: number;
  is_edited: boolean;
  // Inline thumbnail data URL; the current backend sends file paths instead.
  data?: string;
}

interface ImageMetadataLoadedPayload {
  path: string;
  rating: number;
  is_edited: boolean;
  tags: string[] | null;
}

interface PathProgressPayload {
  current: number;
  total: number;
  path: string;
}


interface MergeCompletePayload {
  job: number;
  base64: string;
  overlay: string | null;
  report: Record<string, unknown>;
  width: number;
  height: number;
}

interface TauriListenerProps {
  refreshAllFolderTrees: () => void;
  handleSelectSubfolder: (path: string, isNewRoot?: boolean, preloadedImages?: ImageFile[], expandParents?: boolean) => void;
  refreshImageList: () => void;
  markGenerated: (path: string) => void;
}

/** New grid thumbnails taken into the store per animation frame. */
export const THUMBNAILS_PER_FRAME = 12;

export function useTauriListeners({
  refreshAllFolderTrees,
  handleSelectSubfolder,
  refreshImageList,
  markGenerated,
}: TauriListenerProps) {
  const refs = useRef({ refreshAllFolderTrees, handleSelectSubfolder, refreshImageList, markGenerated });

  useEffect(() => {
    refs.current = { refreshAllFolderTrees, handleSelectSubfolder, refreshImageList, markGenerated };
  });

  const thumbnailBuffer = useRef<Record<string, string>>({});
  const mediumThumbnailBuffer = useRef<Record<string, string>>({});
  const ratingBuffer = useRef<Record<string, number>>({});
  const editStatusBuffer = useRef<Record<string, boolean>>({});
  const flushHandle = useRef<number | null>(null);

  useEffect(() => {
    // Exports a quit or crash interrupted are kept for resuming.
    invoke<ExportJob[]>(Invokes.ExportQueueJobs)
      .then((jobs) => {
        if (!Array.isArray(jobs)) return;
        useProcessStore.getState().setProcess({ exportQueue: jobs });
        const interrupted = jobs.filter((job) => job.state === 'interrupted');
        if (interrupted.length)
          toast.info(
            interrupted.length === 1
              ? `An export was interrupted (${interrupted[0].done} of ${interrupted[0].total} done). Resume it in Export.`
              : `${interrupted.length} exports were interrupted. Resume them in Export.`,
          );
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    let isEffectActive = true;

    const flushThumbnailBatch = () => {
      flushHandle.current = null;
      if (!isEffectActive) return;

      // Reveal at most a few new thumbnails per frame: a whole burst of new
      // images loading and painting together made the first grid seconds of
      // large sessions drop frames (53–69 ms with 5,000 RAWs).
      const thumbPaths = Object.keys(thumbnailBuffer.current);
      const now = thumbPaths.slice(0, THUMBNAILS_PER_FRAME);
      const pendingThumbs: Record<string, string> = {};
      const pendingMediumThumbs: Record<string, string> = {};
      for (const path of now) {
        pendingThumbs[path] = thumbnailBuffer.current[path];
        if (mediumThumbnailBuffer.current[path]) pendingMediumThumbs[path] = mediumThumbnailBuffer.current[path];
        delete thumbnailBuffer.current[path];
        delete mediumThumbnailBuffer.current[path];
      }
      const pendingRatings = ratingBuffer.current;
      const pendingEdits = editStatusBuffer.current;

      ratingBuffer.current = {};
      editStatusBuffer.current = {};
      if (thumbPaths.length > now.length) scheduleFlush();

      if (Object.keys(pendingThumbs).length > 0) {
        useProcessStore.getState().setProcess((state) => ({
          thumbnails: { ...state.thumbnails, ...pendingThumbs },
          mediumThumbnails: { ...state.mediumThumbnails, ...pendingMediumThumbs },
        }));
        useEditorStore.getState().setEditor((state) => {
          const image = state.selectedImage;
          const thumbnailUrl = image && pendingMediumThumbs[image.path];
          if (!image || image.isReady || image.thumbnailSource === 'embedded' || state.hasRenderedFirstFrame || !thumbnailUrl
              || image.thumbnailUrl === thumbnailUrl) return state;
          return { selectedImage: { ...image, thumbnailUrl } };
        });
      }

      if (Object.keys(pendingRatings).length > 0 || Object.keys(pendingEdits).length > 0) {
        useLibraryStore.getState().setLibrary((state) => {
          const indices = libraryImageIndex(state.imageList);
          // Most thumbnail events repeat the existing 0/false metadata. Keep
          // references stable so a 20k-image library is not sorted again per tile.
          const changedRatings = Object.entries(pendingRatings).filter(
            ([path, rating]) =>
              (state.imageRatings[path] ?? state.imageList[indices.get(path) ?? -1]?.rating ?? 0) !== rating,
          );
          const changedEdits = Object.entries(pendingEdits).filter(([path, edited]) => {
            const image = state.imageList[indices.get(path) ?? -1];
            return image && Boolean(image.is_edited) !== edited;
          });
          const hasChangedEdits = changedEdits.length > 0;
          if (!changedRatings.length && !hasChangedEdits) return state;
          const imageList = hasChangedEdits ? state.imageList.slice() : state.imageList;
          for (const [path, is_edited] of changedEdits) {
            const index = indices.get(path)!;
            imageList[index] = { ...imageList[index], is_edited };
          }
          return {
            imageRatings: changedRatings.length
              ? { ...state.imageRatings, ...Object.fromEntries(changedRatings) }
              : state.imageRatings,
            imageList,
          };
        });
      }
    };

    const scheduleFlush = () => {
      if (flushHandle.current !== null) return;
      flushHandle.current = requestAnimationFrame(flushThumbnailBatch);
    };

    const listeners = [
      listen<string>('preview-update-uncropped', (event) => {
        if (isEffectActive) useEditorStore.getState().setEditor({ uncroppedAdjustedPreviewUrl: event.payload });
      }),
      listen<AnalyticsUpdatePayload>('analytics-update', (event) => {
        if (isEffectActive && event.payload.path === useEditorStore.getState().selectedImage?.path) {
          const update: Pick<Partial<EditorState>, 'histogram' | 'waveform'> = {};
          if (event.payload.histogram != null) update.histogram = event.payload.histogram;
          if (event.payload.waveform != null) update.waveform = event.payload.waveform;
          useEditorStore.getState().setEditor(update);
        }
      }),
      listen<string>('open-with-file', (event) => {
        if (isEffectActive) useProcessStore.getState().setProcess({ initialFileToOpen: event.payload });
      }),
      listen<ExternalEditSession>('external-edit-session', (event) => {
        if (isEffectActive) useProcessStore.getState().setProcess({ externalEditSession: event.payload });
      }),
      listen<Progress>('thumbnail-progress', (event) => {
        if (isEffectActive)
          useProcessStore
            .getState()
            .setProcess({ thumbnailProgress: { current: event.payload.current, total: event.payload.total } });
      }),
      listen('thumbnail-generation-complete', () => {
        if (isEffectActive) useProcessStore.getState().setProcess({ thumbnailProgress: { current: 0, total: 0 } });
      }),
      listen<ThumbnailGeneratedPayload>('thumbnail-generated', (event) => {
        if (!isEffectActive) return;
        const { path, thumbnailPath, previewPath, rating, is_edited, data } = event.payload;

        if (thumbnailPath && previewPath) {
          thumbnailBuffer.current[path] = convertFileSrc(thumbnailPath.replace(/\\/g, '/'));
          mediumThumbnailBuffer.current[path] = convertFileSrc(previewPath.replace(/\\/g, '/'));
          refs.current.markGenerated(path);
        } else if (data) {
          thumbnailBuffer.current[path] = data;
          mediumThumbnailBuffer.current[path] = data;
          refs.current.markGenerated(path);
        }
        if (rating !== undefined) {
          ratingBuffer.current[path] = rating;
        }
        if (is_edited !== undefined) {
          editStatusBuffer.current[path] = is_edited;
        }
        if (thumbnailPath || data || rating !== undefined || is_edited !== undefined) {
          scheduleFlush();
        }
      }),
      listen<ImageMetadataLoadedPayload>('image-metadata-loaded', (event) => {
        if (!isEffectActive) return;
        const { path, rating, is_edited, tags } = event.payload;

        useLibraryStore.getState().setLibrary((state) => {
          const index = libraryImageIndex(state.imageList).get(path) ?? -1;
          const image = state.imageList[index];
          const ratingChanged = rating !== undefined && (state.imageRatings[path] ?? image?.rating ?? 0) !== rating;
          const tagsChanged = image && tags != null &&
            (tags.length !== (image.tags?.length ?? 0) || tags.some((tag: string, i: number) => tag !== image.tags?.[i]));
          const imageChanged = image && (Boolean(image.is_edited) !== Boolean(is_edited) || tagsChanged);
          // Opening an already indexed photo emits the same metadata again.
          // Preserve list/ratings identity so that this cannot re-query 20k files
          // on the critical path to the first developed frame.
          if (!ratingChanged && !imageChanged) return state;
          const imageList = imageChanged ? state.imageList.slice() : state.imageList;
          if (imageChanged) imageList[index] = { ...image, is_edited, tags: tags ?? image.tags };
          return {
            imageRatings: ratingChanged ? { ...state.imageRatings, [path]: rating } : state.imageRatings,
            imageList,
          };
        });
      }),
      listen<string>('ai-model-download-start', (event) => {
        if (isEffectActive) useProcessStore.getState().setProcess({ aiModelDownloadStatus: event.payload });
      }),
      listen('ai-model-download-finish', () => {
        if (isEffectActive) useProcessStore.getState().setProcess({ aiModelDownloadStatus: null });
      }),
      listen('indexing-started', () => {
        if (isEffectActive)
          useProcessStore.getState().setProcess({ isIndexing: true, indexingProgress: { current: 0, total: 0 } });
      }),
      listen<Progress>('indexing-progress', (event) => {
        if (isEffectActive) useProcessStore.getState().setProcess({ indexingProgress: event.payload });
      }),
      listen('indexing-finished', () => {
        if (isEffectActive) {
          useProcessStore.getState().setProcess({ isIndexing: false, indexingProgress: { current: 0, total: 0 } });
          const currentPath = useLibraryStore.getState().currentFolderPath;
          if (currentPath) {
            refs.current.refreshImageList();
          }
        }
      }),
      listen<PathProgressPayload>('batch-export-progress', (event) => {
        if (isEffectActive) useProcessStore.getState().setExportState({ progress: event.payload });
      }),
      listen<{ current: number; total: number }>('export-job-started', (event) => {
        if (isEffectActive)
          useProcessStore.getState().setExportState({
            status: Status.Exporting,
            errorMessage: '',
            progress: { current: event.payload.current, total: event.payload.total },
          });
      }),
      listen<ExportJob[]>('export-queue-changed', (event) => {
        if (isEffectActive) useProcessStore.getState().setProcess({ exportQueue: event.payload });
      }),
      listen<{ destination: string | null; skipped: boolean; error: string | null }>('hot-folder-imported', (event) => {
        if (!isEffectActive) return;
        const { destination, skipped, error } = event.payload;
        if (!destination || skipped || error) return;
        const { currentFolderPath, setLibrary } = useLibraryStore.getState();
        const folder = destination.slice(0, Math.max(destination.lastIndexOf('/'), destination.lastIndexOf('\\')));
        void refs.current.refreshAllFolderTrees();
        if (currentFolderPath !== folder) return;
        void Promise.resolve(refs.current.refreshImageList()).then(() => {
          // The new capture is selected in the Library, as in Capture One.
          if (useUIStore.getState().activeView !== 'editor')
            setLibrary({ multiSelectedPaths: [destination], libraryActivePath: destination });
        });
      }),
      // The files moved but the session/catalog database could not follow
      // (e.g. locked); it is retried with the next file operation or start.
      listen<{ label: string; error: string }>('file-operation-sync-failed', (event) => {
        if (isEffectActive)
          toast.warning(
            `${event.payload.label}: the files were changed, but the catalog could not be updated ` +
              `(${event.payload.error}). Rawtonic will retry with the next file operation or start.`,
            { autoClose: false },
          );
      }),
      // The catalog caught up on such an operation (file_operations.rs
      // `FILE_OPERATIONS_CAUGHT_UP`): the list, its picks and the offline
      // and missing counts are read again.
      listen<string[]>('file-operations-caught-up', () => {
        if (!isEffectActive) return;
        refs.current.refreshImageList();
        void recountAfterFileOperation();
      }),
      listen('export-complete', () => {
        if (isEffectActive) useProcessStore.getState().setExportState({ status: Status.Success });
      }),
      listen<string>('export-error', (event) => {
        if (isEffectActive)
          useProcessStore.getState().setExportState({
            status: Status.Error,
            errorMessage: typeof event.payload === 'string' ? event.payload : 'Unknown error',
          });
      }),
      listen('export-cancelling', () => {
        if (isEffectActive) useProcessStore.getState().setExportState({ status: Status.Cancelling });
      }),
      listen('export-cancelled', () => {
        if (isEffectActive) useProcessStore.getState().setExportState({ status: Status.Cancelled });
      }),
      listen<{ total: number }>('import-start', (event) => {
        if (isEffectActive)
          useProcessStore.getState().setImportState({
            errorMessage: '',
            path: '',
            progress: { current: 0, total: event.payload.total },
            status: Status.Importing,
          });
      }),
      listen<PathProgressPayload>('import-progress', (event) => {
        if (isEffectActive)
          useProcessStore.getState().setImportState({
            path: event.payload.path,
            progress: { current: event.payload.current, total: event.payload.total },
          });
      }),
      listen('import-complete', () => {
        if (isEffectActive) {
          useProcessStore.getState().setImportState({ status: Status.Success });
          refs.current.refreshAllFolderTrees();
          const currentPath = useLibraryStore.getState().currentFolderPath;
          if (currentPath) {
            refs.current.handleSelectSubfolder(currentPath, false);
          }
        }
      }),
      listen<string>('import-error', (event) => {
        if (isEffectActive)
          useProcessStore.getState().setImportState({
            status: Status.Error,
            errorMessage: typeof event.payload === 'string' ? event.payload : 'Unknown error',
          });
      }),
      listen<{ path: string }>('wgpu-frame-ready', (event) => {
        if (isEffectActive && event.payload?.path === useEditorStore.getState().selectedImage?.path) {
          useEditorStore.getState().setEditor({ hasRenderedFirstFrame: true, holdingNativeFrame: false });
        }
      }),
      // Closing the window or Quit: save what is pending first.
      listen('app-quit-requested', () => {
        if (isEffectActive) {
          handleQuitRequest(confirmQuitWithUnsaved).catch((err) => console.error('Quitting failed:', err));
        }
      }),
      // Merge events of a run the dialog no longer waits for are ignored.
      listen<{ job: number; message: string }>('merge-progress', (event) => {
        if (isEffectActive) {
          setMergeStateFor(event.payload.job, () => ({ progressMessage: event.payload.message }));
        }
      }),
      listen<MergeCompletePayload>('merge-complete', (event) => {
        if (isEffectActive) {
          setMergeStateFor(event.payload.job, () => ({
            error: null,
            isProcessing: false,
            overlayBase64: event.payload.overlay ?? null,
            previewBase64: event.payload.base64,
            progressMessage: null,
            report: { ...event.payload.report, width: event.payload.width, height: event.payload.height },
          }));
        }
      }),
      listen<{ job: number; error: unknown }>('merge-error', (event) => {
        if (isEffectActive) {
          setMergeStateFor(event.payload.job, () => ({
            error: String(event.payload.error),
            isProcessing: false,
            previewBase64: null,
            progressMessage: null,
          }));
        }
      }),
      listen<number>('culling-start', (event) => {
        if (isEffectActive) {
          useUIStore.getState().setUI((state) => ({
            cullingModalState: {
              ...state.cullingModalState,
              isOpen: true,
              progress: { current: 0, total: event.payload, stage: 'Initializing...' },
              suggestions: null,
              error: null,
            },
          }));
        }
      }),
      listen<{ current: number; total: number; stage: string }>('culling-progress', (event) => {
        if (isEffectActive) {
          useUIStore
            .getState()
            .setUI((state) => ({ cullingModalState: { ...state.cullingModalState, progress: event.payload } }));
        }
      }),
      listen<CullingSuggestions>('culling-complete', (event) => {
        if (isEffectActive) {
          useUIStore.getState().setUI((state) => ({
            cullingModalState: { ...state.cullingModalState, progress: null, suggestions: event.payload },
          }));
        }
      }),
      listen<unknown>('culling-error', (event) => {
        if (isEffectActive) {
          useUIStore.getState().setUI((state) => ({
            cullingModalState: { ...state.cullingModalState, progress: null, error: String(event.payload) },
          }));
        }
      }),
    ];

    return () => {
      isEffectActive = false;
      if (flushHandle.current !== null) {
        cancelAnimationFrame(flushHandle.current);
        flushHandle.current = null;
      }
      thumbnailBuffer.current = {};
      ratingBuffer.current = {};
      listeners.forEach((p) => p.then((unlisten) => unlisten()));
    };
  }, []);
}
