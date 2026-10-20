import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import type { Adjustments } from '../utils/adjustments';

/** A named snapshot of a photograph's development (`rawtonic_catalog::Snapshot`). */
export interface Snapshot {
  id: string;
  name: string;
  /** RFC 3339, UTC. */
  createdAt: string;
  adjustments: Adjustments;
}

interface SnapshotState {
  /** The photograph (or virtual copy) the list belongs to. */
  path: string | null;
  snapshots: Snapshot[];
  /** Thumbnails by snapshot id (object URLs), developed one at a time. */
  thumbnails: Record<string, string>;
  /** Reads the list of `path` (null: none shown). */
  load: (path: string | null) => Promise<void>;
  create: (path: string, name: string, adjustments: Adjustments) => Promise<Snapshot | null>;
  rename: (path: string, id: string, name: string) => Promise<void>;
  /** Gives the snapshot new adjustments (Lightroom's Update with Current Settings). */
  update: (path: string, id: string, adjustments: Adjustments) => Promise<void>;
  remove: (path: string, id: string) => Promise<void>;
}

/** The photograph whose thumbnails are being developed. */
let developing: string | null = null;

/** Develops the missing thumbnails of the shown list, one after another
 * (the open photograph's renders go first between them); snapshots taken
 * meanwhile join the same run. */
async function developThumbnails() {
  const path = useSnapshotStore.getState().path;
  if (!path || developing === path) return;
  developing = path;
  try {
    for (;;) {
      const state = useSnapshotStore.getState();
      const next = state.snapshots.find((snapshot) => !(snapshot.id in state.thumbnails));
      if (state.path !== path || !next) return;
      let url = '';
      try {
        const bytes = await invoke<ArrayBuffer>(Invokes.SnapshotThumbnail, { path, adjustments: next.adjustments });
        url = URL.createObjectURL(new Blob([bytes], { type: 'image/jpeg' }));
      } catch (error) {
        console.error('Snapshot thumbnail failed:', error);
      }
      if (useSnapshotStore.getState().path !== path) {
        if (url) URL.revokeObjectURL(url);
        return;
      }
      // A failed one stays empty (a placeholder) rather than being retried.
      useSnapshotStore.setState((current) => ({ thumbnails: { ...current.thumbnails, [next.id]: url } }));
    }
  } finally {
    if (developing === path) developing = null;
  }
}

/** Shows the list the backend answered for `path` (an answer that is no
 * list shows none). */
function show(path: string, answer: unknown) {
  const snapshots: Snapshot[] = Array.isArray(answer) ? answer : [];
  const state = useSnapshotStore.getState();
  if (state.path !== path) return;
  // Thumbnails of snapshots no longer listed are released.
  const kept: Record<string, string> = {};
  for (const [id, url] of Object.entries(state.thumbnails)) {
    if (snapshots.some((snapshot) => snapshot.id === id)) kept[id] = url;
    else if (url) URL.revokeObjectURL(url);
  }
  useSnapshotStore.setState({ snapshots, thumbnails: kept });
  void developThumbnails();
}

export const useSnapshotStore = create<SnapshotState>((set, get) => ({
  path: null,
  snapshots: [],
  thumbnails: {},

  load: async (path) => {
    if (get().path !== path) {
      Object.values(get().thumbnails).forEach((url) => url && URL.revokeObjectURL(url));
      set({ path, snapshots: [], thumbnails: {} });
    }
    if (!path) return;
    show(path, await invoke<unknown>(Invokes.SnapshotsList, { path }));
  },

  create: async (path, name, adjustments) => {
    const before = new Set(get().path === path ? get().snapshots.map((s) => s.id) : []);
    show(path, await invoke<unknown>(Invokes.SnapshotCreate, { path, name, adjustments }));
    return get().path === path ? (get().snapshots.find((snapshot) => !before.has(snapshot.id)) ?? null) : null;
  },

  rename: async (path, id, name) => {
    show(path, await invoke<unknown>(Invokes.SnapshotRename, { path, id, name }));
  },

  update: async (path, id, adjustments) => {
    const answer = await invoke<unknown>(Invokes.SnapshotUpdate, { path, id, adjustments });
    // Its thumbnail is developed again.
    const url = get().thumbnails[id];
    if (get().path === path && url !== undefined) {
      if (url) URL.revokeObjectURL(url);
      const { [id]: _old, ...rest } = get().thumbnails;
      set({ thumbnails: rest });
    }
    show(path, answer);
  },

  remove: async (path, id) => {
    show(path, await invoke<unknown>(Invokes.SnapshotDelete, { path, id }));
  },
}));
