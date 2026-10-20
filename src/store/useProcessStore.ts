import { create } from 'zustand';
import { Progress } from '../components/ui/AppProperties';
import { ExportState, Status } from '../components/ui/ExportImportProperties';

export interface ExternalEditSession {
  source: string;
  output: string;
  format: string;
  jpegQuality: number;
}

/** Mirrors `rawtonic_export::queue::Job`. */
export interface ExportJob {
  id: string;
  label: string;
  state: 'queued' | 'running' | 'done' | 'failed' | 'cancelled' | 'interrupted';
  createdAt: string;
  total: number;
  done: number;
  failed: number;
  /** The photos that did not export and the reasons (export_queue.rs). */
  failures: ExportFailure[];
}

export interface ExportFailure {
  path: string;
  error: string;
}

export interface AiTask {
  id: string;
  type: string;
  /** Requests running under this id: the same component (copied settings
   * keep its id) may compute on two photos at once. */
  count: number;
}

interface ProcessState {
  exportState: ExportState;
  /** The persistent export queue (running, waiting and resumable jobs). */
  exportQueue: ExportJob[];
  isIndexing: boolean;
  indexingProgress: Progress;
  thumbnails: Record<string, string>;
  mediumThumbnails: Record<string, string>;
  thumbnailProgress: Progress;
  previews: Record<string, { url: string; thumbKey: string; timestamp: number }>;
  aiModelDownloadStatus: string | null;
  copiedFilePaths: Array<string>;
  isCopied: boolean;
  isPasted: boolean;
  initialFileToOpen: string | null;
  externalEditSession: ExternalEditSession | null;
  activeAiTasks: Record<string, AiTask>;

  setProcess: (state: Partial<ProcessState> | ((state: ProcessState) => Partial<ProcessState>)) => void;
  setExportState: (updater: Partial<ExportState> | ((state: ExportState) => Partial<ExportState>)) => void;
  setPreview: (path: string, url: string, thumbKey: string) => void;
  clearPreviews: () => void;
  registerAiTask: (id: string, type: string) => void;
  unregisterAiTask: (id: string) => void;
  isAiTaskRunning: (id: string) => boolean;
}

let exportTimeout: ReturnType<typeof setTimeout>;
let copyTimeout: ReturnType<typeof setTimeout>;
let pasteTimeout: ReturnType<typeof setTimeout>;

const MAX_PREVIEW_CACHE_SIZE = 10;

export const useProcessStore = create<ProcessState>((set, get) => ({
  exportState: { errorMessage: '', progress: { current: 0, total: 0 }, status: Status.Idle },
  exportQueue: [],
  isIndexing: false,
  indexingProgress: { current: 0, total: 0 },
  thumbnails: {},
  mediumThumbnails: {},
  thumbnailProgress: { current: 0, total: 0 },
  previews: {},
  aiModelDownloadStatus: null,
  copiedFilePaths: [],
  isCopied: false,
  isPasted: false,
  initialFileToOpen: null,
  externalEditSession: null,
  activeAiTasks: {},

  setProcess: (updater) => {
    set((prev) => {
      const nextState = typeof updater === 'function' ? updater(prev) : updater;
      return { ...prev, ...nextState };
    });

    const state = get();
    if (state.isCopied) {
      clearTimeout(copyTimeout);
      copyTimeout = setTimeout(() => set({ isCopied: false }), 1000);
    }
    if (state.isPasted) {
      clearTimeout(pasteTimeout);
      pasteTimeout = setTimeout(() => set({ isPasted: false }), 1000);
    }
  },

  setExportState: (updater) => {
    set((prev) => ({
      exportState: { ...prev.exportState, ...(typeof updater === 'function' ? updater(prev.exportState) : updater) },
    }));

    const status = get().exportState.status;

    clearTimeout(exportTimeout);

    if ([Status.Success, Status.Error, Status.Cancelled].includes(status)) {
      exportTimeout = setTimeout(() => {
        set((prev) => ({
          exportState: {
            ...prev.exportState,
            status: Status.Idle,
            denoising: [],
            errorMessage: '',
            progress: { current: 0, total: 0 },
          },
        }));
      }, 5000);
    }
  },

  setPreview: (path, url, thumbKey) => {
    set((state) => {
      const newPreviews = { ...state.previews };

      if (newPreviews[path] && newPreviews[path].url !== url) {
        URL.revokeObjectURL(newPreviews[path].url);
      }

      newPreviews[path] = { url, thumbKey, timestamp: Date.now() };

      const keys = Object.keys(newPreviews);
      if (keys.length > MAX_PREVIEW_CACHE_SIZE) {
        let oldestPath = keys[0];
        let oldestTime = newPreviews[oldestPath].timestamp;

        for (let i = 1; i < keys.length; i++) {
          const key = keys[i];
          if (newPreviews[key].timestamp < oldestTime) {
            oldestTime = newPreviews[key].timestamp;
            oldestPath = key;
          }
        }

        if (oldestPath !== path) {
          URL.revokeObjectURL(newPreviews[oldestPath].url);
          delete newPreviews[oldestPath];
        }
      }

      return { previews: newPreviews };
    });
  },

  clearPreviews: () => {
    set((state) => {
      Object.values(state.previews).forEach((p) => URL.revokeObjectURL(p.url));
      return { previews: {} };
    });
  },

  registerAiTask: (id, type) =>
    set((state) => ({
      activeAiTasks: { ...state.activeAiTasks, [id]: { id, type, count: (state.activeAiTasks[id]?.count ?? 0) + 1 } },
    })),

  // The task ends with the last request under its id.
  unregisterAiTask: (id) =>
    set((state) => {
      const next = { ...state.activeAiTasks };
      const task = next[id];
      if (task && task.count > 1) next[id] = { ...task, count: task.count - 1 };
      else delete next[id];
      return { activeAiTasks: next };
    }),

  isAiTaskRunning: (id) => !!get().activeAiTasks[id],
}));
