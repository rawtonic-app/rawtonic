import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import i18n from 'i18next';
import { CullingSuggestions, ImageFile, Invokes, Progress } from '../components/ui/AppProperties';
import { Status } from '../components/ui/ExportImportProperties';
import {
  ExportItemProgress,
  ExportQueueSnapshot,
  ExternalEditSession,
  useProcessStore,
} from '../store/useProcessStore';
import { followsExportItems, loadExportItems } from '../components/studio/StudioExportQueue';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { libraryImageIndex } from '../utils/libraryIndex';
import { setMergeStateFor } from './useProductivityActions';
import { confirmQuitWithUnsaved, handleQuitRequest } from '../utils/quit';
import { recountAfterFileOperation } from '../utils/offline';
import { throttleAsync } from '../utils/throttle';
import { IMPORTS_CHANGED } from '../utils/folderSync';
import { metadataWritten } from '../utils/catalogWrites';
import { globalImageCache } from '../utils/ImageLRUCache';

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
  /** Null when no sidecar was read (the listing knows them). */
  rating: number | null;
  is_edited: boolean | null;
  // Inline thumbnail data URL; the current backend sends file paths instead.
  data?: string;
}

interface ImageMetadataLoadedPayload {
  path: string;
  rating: number;
  is_edited: boolean;
  tags: string[] | null;
}

/** Ratings a batch of the facts pass took from the camera (`library_search`):
 * (path, rating) pairs, paths as the catalog holds them. */
type CameraRatingsPayload = [string, number][];

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
  handleSelectSubfolder: (
    path: string,
    isNewRoot?: boolean,
    preloadedImages?: ImageFile[],
    expandParents?: boolean,
    preserveEditor?: boolean,
  ) => void | Promise<void>;
  refreshImageList: () => void;
  markGenerated: (path: string) => void;
  /** Opens a photograph (in Develop with `openInEditor`). */
  handleImageSelect?: (path: string, openInEditor?: boolean) => void;
}

/** How often the grid lists its folder again while an ingest copies
 * photographs into it (at most four times a second), and how often the
 * folder counts follow. */
export const INGEST_LIST_REFRESH_MS = 250;
export const INGEST_TREE_REFRESH_MS = 1000;

const parentFolder = (path: string) => path.slice(0, Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\')));

/** New grid thumbnails taken into the store per animation frame. */
export const THUMBNAILS_PER_FRAME = 12;

export function useTauriListeners({
  refreshAllFolderTrees,
  handleSelectSubfolder,
  refreshImageList,
  markGenerated,
  handleImageSelect,
}: TauriListenerProps) {
  const refs = useRef({ refreshAllFolderTrees, handleSelectSubfolder, refreshImageList, markGenerated, handleImageSelect });
  const latestCapture = useRef(0);

  useEffect(() => {
    refs.current = { refreshAllFolderTrees, handleSelectSubfolder, refreshImageList, markGenerated, handleImageSelect };
  });

  const thumbnailBuffer = useRef<Record<string, string>>({});
  const mediumThumbnailBuffer = useRef<Record<string, string>>({});
  const ratingBuffer = useRef<Record<string, number>>({});
  const editStatusBuffer = useRef<Record<string, boolean>>({});
  const flushHandle = useRef<number | null>(null);

  /** A capture the hot folder or the tether brought in: it joined their
   * import batch (Previous Import) and is selected in the Library, as in
   * Capture One — with `inDevelop` (tether), also opened when Develop is
   * showing, and its folder shown when another one is. */
  const showCapture = (
    {
      destination,
      skipped,
      error,
      session,
    }: { destination: string | null; skipped: boolean; error: string | null; session?: string },
    inDevelop: boolean,
  ) => {
    if (!destination || skipped || error) return;
    // A capture of another library (still arriving after a switch) does not
    // move this one's view.
    const sameLibrary = () => {
      const open = useLibraryStore.getState().sessionDirectory;
      return !session || !open || session.replace(/[\\/]$/, '') === open.replace(/[\\/]$/, '');
    };
    if (!sameLibrary()) return;
    // Only the newest capture is shown once the views have caught up (an
    // earlier one's slower refresh must not bring it back).
    const capture = ++latestCapture.current;
    window.dispatchEvent(new Event(IMPORTS_CHANGED));
    const { currentFolderPath, setLibrary } = useLibraryStore.getState();
    const folder = destination.slice(0, Math.max(destination.lastIndexOf('/'), destination.lastIndexOf('\\')));
    void refs.current.refreshAllFolderTrees();
    // A tethered capture brings its folder into view (Capture One's viewer
    // follows the captures); a hot folder capture does not move the view.
    const shown =
      currentFolderPath === folder
        ? Promise.resolve(refs.current.refreshImageList())
        : inDevelop
          ? // Develop stays open while its folder changes (Capture One's
            // viewer keeps showing the newest capture).
            Promise.resolve(
              refs.current.handleSelectSubfolder(
                folder,
                false,
                undefined,
                true,
                useUIStore.getState().activeView === 'editor',
              ),
            )
          : null;
    if (!shown) return;
    void shown.then(() => {
      if (capture !== latestCapture.current || !sameLibrary()) return;
      // The folder change did happen (a later navigation may have cut it).
      if (useLibraryStore.getState().currentFolderPath !== folder) return;
      if (useUIStore.getState().activeView !== 'editor')
        setLibrary({ multiSelectedPaths: [destination], libraryActivePath: destination });
      else if (inDevelop) refs.current.handleImageSelect?.(destination, true);
    });
  };

  useEffect(() => {
    // Exports a quit or crash interrupted are kept for resuming.
    invoke<ExportQueueSnapshot>(Invokes.ExportQueueJobs)
      .then((snapshot) => {
        if (!Array.isArray(snapshot?.jobs)) return;
        useProcessStore.getState().setExportQueue(snapshot);
        const interrupted = snapshot.jobs.filter((job) => job.state === 'interrupted');
        if (interrupted.length)
          toast.info(
            interrupted.length === 1
              ? `An export was interrupted (${interrupted[0].done} of ${interrupted[0].total} done). Resume it in the export queue.`
              : `${interrupted.length} exports were interrupted. Resume them in the export queue.`,
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
          // A thumbnail's rating was read from the sidecar when its generation
          // began, so it only fills a rating the listing did not bring: a
          // later listing or rating (a photograph followed by Synchronize
          // Folder, rated meanwhile) is newer and stays.
          const changedRatings = Object.entries(pendingRatings).filter(
            ([path, rating]) =>
              state.imageRatings[path] === undefined &&
              state.imageList[indices.get(path) ?? -1]?.rating === undefined &&
              rating !== 0,
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

    // An ingest's photographs appear in the grid as each is copied
    // (`session-ingest-imported`), the list read again at most every
    // INGEST_LIST_REFRESH_MS, so the user browses and opens them meanwhile.
    const ingestFolders = new Set<string>();
    const refreshIngestList = throttleAsync(async () => {
      const folders = new Set(ingestFolders);
      ingestFolders.clear();
      const { currentFolderPath } = useLibraryStore.getState();
      if (isEffectActive && currentFolderPath && folders.has(currentFolderPath))
        await refs.current.refreshImageList();
    }, INGEST_LIST_REFRESH_MS);
    const refreshIngestTree = throttleAsync(
      () => isEffectActive && refs.current.refreshAllFolderTrees(),
      INGEST_TREE_REFRESH_MS,
    );

    const listeners = [
      listen<{ destination: string | null; skipped: boolean; error: string | null }>(
        'session-ingest-imported',
        (event) => {
          if (!isEffectActive) return;
          const { destination, skipped, error } = event.payload;
          if (!destination || skipped || error) return;
          ingestFolders.add(parentFolder(destination));
          refreshIngestList();
          refreshIngestTree();
        },
      ),
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
      // Another program saved an Edit With render (open_with.rs): its kept
      // previews are stale (the new thumbnail follows as thumbnail-generated),
      // and Develop shows it again when it is open there.
      listen<{ path: string }>('edit-with-saved', (event) => {
        if (!isEffectActive) return;
        const { path } = event.payload;
        globalImageCache.deleteByPrefix(path);
        const shown = useEditorStore.getState().selectedImage?.path;
        if (shown && shown.split('?vc=')[0] === path && useUIStore.getState().activeView === 'editor')
          refs.current.handleImageSelect?.(shown, true);
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
        if (rating != null) {
          ratingBuffer.current[path] = rating;
        }
        if (is_edited != null) {
          editStatusBuffer.current[path] = is_edited;
        }
        if (thumbnailPath || data || rating != null || is_edited != null) {
          scheduleFlush();
        }
      }),
      listen<CameraRatingsPayload>('library-camera-ratings', async (event) => {
        if (!isEffectActive) return;
        let ratings = event.payload;
        const { imageList, currentFolderPath } = useLibraryStore.getState();
        const listed = libraryImageIndex(imageList);
        if (currentFolderPath && ratings.some(([path]) => !listed.has(path))) {
          // A folder browsed through a symlink lists its photographs in its
          // own form.
          const paths = await invoke<string[]>(Invokes.SessionPathsAsListed, {
            paths: ratings.map(([path]) => path),
            listed: currentFolderPath,
          }).catch(() => null);
          if (!isEffectActive) return;
          if (paths?.length === ratings.length) ratings = ratings.map(([, rating], i) => [paths[i], rating]);
        }
        // A camera's rating applies only to a photograph listed and still
        // unrated here that this window has not written; one update a batch.
        useLibraryStore.getState().setLibrary((state) => {
          const index = libraryImageIndex(state.imageList);
          let imageRatings: Record<string, number> | null = null;
          for (const [path, rating] of ratings) {
            const image = state.imageList[index.get(path) ?? -1];
            if (!image || metadataWritten(path)) continue;
            if (((imageRatings ?? state.imageRatings)[path] ?? image.rating ?? 0) !== 0) continue;
            imageRatings ??= { ...state.imageRatings };
            imageRatings[path] = rating;
          }
          return imageRatings ? { imageRatings } : state;
        });
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
        if (isEffectActive)
          useProcessStore.getState().setExportState((state) => ({
            denoising: state.denoising?.filter((path) => path !== event.payload.path),
            progress: event.payload,
          }));
      }),
      listen<string>('export-denoising', (event) => {
        if (isEffectActive)
          useProcessStore.getState().setExportState((state) => ({
            denoising: [...(state.denoising ?? []), event.payload],
          }));
      }),
      // `ids`: every job of the run (recipes sharing each photo's develop).
      listen<{ id: string; ids?: string[]; current: number; total: number }>('export-job-started', (event) => {
        if (!isEffectActive) return;
        useProcessStore.getState().setExportState({
          denoising: [],
          status: Status.Exporting,
          errorMessage: '',
          progress: { current: event.payload.current, total: event.payload.total },
        });
        // The running jobs' photos, for their progress and time left.
        for (const id of event.payload.ids ?? (event.payload.id ? [event.payload.id] : [])) void loadExportItems(id);
      }),
      listen<ExportQueueSnapshot>('export-queue-changed', (event) => {
        if (!isEffectActive) return;
        const followed = event.payload.jobs.filter((job) => job.state !== 'running' && followsExportItems(job.id));
        useProcessStore.getState().setExportQueue(event.payload);
        // A run that ended put its unfinished photos back to waiting (no
        // photo event says so); also when the first read is still on its way.
        for (const job of followed) void loadExportItems(job.id);
      }),
      listen<ExportItemProgress>('export-item-progress', (event) => {
        if (isEffectActive) useProcessStore.getState().applyExportItemProgress(event.payload);
      }),
      // Only while the job still runs: a finished job never shows Pausing.
      listen<string>('export-job-pausing', (event) => {
        if (isEffectActive)
          useProcessStore.getState().setProcess((state) =>
            state.exportQueue.some((job) => job.id === event.payload && job.state === 'running')
              ? { exportPausing: [...state.exportPausing, event.payload] }
              : {},
          );
      }),
      listen('export-paused', () => {
        if (isEffectActive)
          useProcessStore.getState().setExportState({ status: Status.Idle, denoising: [], progress: { current: 0, total: 0 } });
      }),
      listen<{ destination: string | null; skipped: boolean; error: string | null }>('hot-folder-imported', (event) => {
        if (isEffectActive) showCapture(event.payload, false);
      }),
      // A tethered capture is shown at once, in Develop too (Capture One
      // follows the newest capture in its viewer).
      listen<{ destination: string | null; skipped: boolean; error: string | null; session: string; cameraName: string }>(
        'tether-captured',
        (event) => {
          if (!isEffectActive) return;
          const { error, cameraName } = event.payload;
          // Said here, not in the tether panel: also with no panel shown
          // (fullscreen).
          if (error) toast.error(i18n.t('session.tether.importFailed', { file: cameraName, error }));
          showCapture(event.payload, true);
        },
      ),
      listen<{ message: string }>('tether-capture-failed', (event) => {
        if (isEffectActive) toast.error(i18n.t('session.tether.captureFailed', { error: event.payload.message }));
      }),
      listen<string>('tether-incomplete', (event) => {
        if (isEffectActive)
          toast.warning(i18n.t('session.tether.incomplete', { path: event.payload }), { autoClose: false });
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
      listen<{ path: string }>('wgpu-frame-ready', (event) => {
        if (isEffectActive && event.payload?.path === useEditorStore.getState().selectedImage?.path) {
          useEditorStore.getState().setEditor({ hasRenderedFirstFrame: true, holdingNativeFrame: false, stillPreview: null });
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
      refreshIngestList.cancel();
      refreshIngestTree.cancel();
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
