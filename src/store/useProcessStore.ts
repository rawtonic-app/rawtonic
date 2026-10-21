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
  state: 'queued' | 'running' | 'done' | 'failed' | 'cancelled' | 'interrupted' | 'paused';
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

/** Mirrors `rawtonic_export::queue::Item`: one photo of an export job. */
export interface ExportItem {
  index: number;
  path: string;
  state: 'pending' | 'running' | 'done' | 'failed';
  output: string | null;
  error: string | null;
  attempts: number;
  /** When its last attempt started and ended (Unix ms). */
  startedMs: number | null;
  finishedMs: number | null;
}

/** What a photo in progress is doing (`export_queue::Stage`). */
export type ExportStage = 'loading' | 'denoising' | 'rendering' | 'masks';

/** A photo as the queue panel shows it, with its live stage. */
export interface ExportItemLive extends ExportItem {
  stage?: ExportStage | null;
  /** How far the stage is (0 … 1): AI Denoise tiles. */
  fraction?: number | null;
  /** `exportEventSeq` of the event that last changed this photo. */
  seq?: number;
}

let exportEventSeq = 0;
/** Bumped by every `export-item-progress`; a read of a job's photos notes
 * it first, so photos an event changed meanwhile keep the event's state. */
export const currentExportEventSeq = () => exportEventSeq;
/** Events for jobs whose photos are not read yet, merged by the first read. */
const unreadExportEvents = new Map<string, Map<number, ExportItemLive>>();
let exportReadSeq = 0;
/** A number for each read of a job's photos, in the order they are asked. */
export const nextExportRead = () => ++exportReadSeq;
/** Per job, the read whose reply was applied last: replies of older reads
 * that arrive later are dropped. */
const appliedExportReads = new Map<string, number>();

/** `export-item-progress`: a photo of a job changed. */
export interface ExportItemProgress {
  job: string;
  item: ExportItem;
  stage: ExportStage | null;
  fraction: number | null;
}

/** `export_queue_jobs` and `export-queue-changed` (export_queue::Snapshot). */
export interface ExportQueueSnapshot {
  jobs: ExportJob[];
  /** Waiting exports do not start until Start queue, an export or a resume. */
  held: boolean;
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
  exportQueueHeld: boolean;
  /** Photos of the jobs the queue panel has shown or that ran, by job id. */
  exportItems: Record<string, ExportItemLive[]>;
  /** Jobs asked to pause that still finish their photos in progress. */
  exportPausing: string[];
  isIndexing: boolean;
  indexingProgress: Progress;
  thumbnails: Record<string, string>;
  mediumThumbnails: Record<string, string>;
  thumbnailProgress: Progress;
  aiModelDownloadStatus: string | null;
  copiedFilePaths: Array<string>;
  isCopied: boolean;
  isPasted: boolean;
  initialFileToOpen: string | null;
  externalEditSession: ExternalEditSession | null;
  activeAiTasks: Record<string, AiTask>;

  setProcess: (state: Partial<ProcessState> | ((state: ProcessState) => Partial<ProcessState>)) => void;
  setExportState: (updater: Partial<ExportState> | ((state: ExportState) => Partial<ExportState>)) => void;
  registerAiTask: (id: string, type: string) => void;
  unregisterAiTask: (id: string) => void;
  isAiTaskRunning: (id: string) => boolean;
  /** `since`: `currentExportEventSeq()` when the read was asked for;
   * `read`: its `nextExportRead()`. */
  setExportItems: (job: string, items: ExportItem[], since?: number, read?: number) => void;
  applyExportItemProgress: (progress: ExportItemProgress) => void;
  setExportQueue: (snapshot: ExportQueueSnapshot) => void;
}

let exportTimeout: ReturnType<typeof setTimeout>;
let copyTimeout: ReturnType<typeof setTimeout>;
let pasteTimeout: ReturnType<typeof setTimeout>;

export const useProcessStore = create<ProcessState>((set, get) => ({
  exportState: { errorMessage: '', progress: { current: 0, total: 0 }, status: Status.Idle },
  exportQueue: [],
  exportQueueHeld: false,
  exportItems: {},
  exportPausing: [],
  isIndexing: false,
  indexingProgress: { current: 0, total: 0 },
  thumbnails: {},
  mediumThumbnails: {},
  thumbnailProgress: { current: 0, total: 0 },
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

  // A reply can arrive after events that changed its photos: those keep the
  // event's state. Live stages survive a reload (the queue does not keep them).
  setExportItems: (job, items, since = exportEventSeq, read = nextExportRead()) =>
    set((state) => {
      if (read < (appliedExportReads.get(job) ?? 0)) return {};
      appliedExportReads.set(job, read);
      const live = new Map((state.exportItems[job] ?? []).map((item) => [item.index, item]));
      for (const [index, item] of unreadExportEvents.get(job) ?? []) live.set(index, item);
      unreadExportEvents.delete(job);
      return {
        exportItems: {
          ...state.exportItems,
          [job]: items.map((item) => {
            const old = live.get(item.index);
            if (old?.seq != null && old.seq > since) return old;
            return item.state === 'running' && old?.state === 'running'
              ? { ...item, stage: old.stage, fraction: old.fraction, seq: old.seq }
              : item;
          }),
        },
      };
    }),

  applyExportItemProgress: ({ job, item, stage, fraction }) =>
    set((state) => {
      const items = state.exportItems[job];
      const live = { ...item, stage, fraction, seq: ++exportEventSeq };
      // Jobs whose photos were not read keep the event for their first read.
      if (!items) {
        const unread = unreadExportEvents.get(job) ?? new Map<number, ExportItemLive>();
        unreadExportEvents.set(job, unread.set(item.index, live));
        return {};
      }
      const next = items.slice();
      const at = next.findIndex((other) => other.index === item.index);
      if (at < 0) next.push(live);
      else next[at] = live;
      return { exportItems: { ...state.exportItems, [job]: next } };
    }),

  setExportQueue: ({ jobs, held }) =>
    set((state) => {
      const ids = new Set(jobs.map((job) => job.id));
      const running = new Set(jobs.filter((job) => job.state === 'running').map((job) => job.id));
      // Removed jobs drop their photos.
      const exportItems = Object.fromEntries(Object.entries(state.exportItems).filter(([id]) => ids.has(id)));
      for (const id of unreadExportEvents.keys()) if (!ids.has(id)) unreadExportEvents.delete(id);
      for (const id of appliedExportReads.keys()) if (!ids.has(id)) appliedExportReads.delete(id);
      return {
        exportQueue: jobs,
        exportQueueHeld: held,
        exportItems,
        exportPausing: state.exportPausing.filter((id) => running.has(id)),
      };
    }),
}));
