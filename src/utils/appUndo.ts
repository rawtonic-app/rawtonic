import { Invokes } from '../components/ui/AppProperties';
import { create } from 'zustand';
import { toast } from 'react-toastify';
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { invokeInOrder } from './catalogWrites';
import { editUndoOrder, nextUndoOrder, onNewEdit } from './undoOrder';

export type CullingValue = { kind: 'rating' | 'flag'; value: number } | { kind: 'color'; value: string | null };
export interface CullingChange {
  path: string;
  assetId?: string | null;
  before: CullingValue;
  after: CullingValue;
}
const changedValue = ({ before, after }: CullingChange) => before.kind !== after.kind || before.value !== after.value;
interface Step {
  order: number;
  label: string;
  changes: CullingChange[];
  directory: string | null;
}
interface CatalogUndoState {
  past: Step[];
  future: Step[];
  busy: boolean;
}
export const useCatalogUndoStore = create<CatalogUndoState>(() => ({ past: [], future: [], busy: false }));
let queue: Promise<unknown> = Promise.resolve();
let branch = 0;

function enqueue<T>(work: () => Promise<T>): Promise<T> {
  const run = queue.then(work);
  queue = run.catch(() => undefined);
  return run;
}

/** Start one catalog step at input time, before waiting for the disk. Native
 * receipts name the actual old values and only the photos committed. */
export function catalogAction(label: string, work: () => Promise<CullingChange[]>): Promise<void> {
  debouncedSetHistory.flush();
  branch++;
  const editor = useEditorStore.getState();
  // A catalog edit branches the same way as a Develop edit.
  editor.setEditor({ history: editor.history.slice(0, editor.historyIndex + 1) });
  const step: Step = {
    order: nextUndoOrder(),
    label,
    changes: [],
    directory: useLibraryStore.getState().sessionDirectory,
  };
  useCatalogUndoStore.setState((s) => ({ past: [...s.past, step].slice(-100), future: [] }));
  return enqueue(async () => {
    try {
      step.changes = ((await work()) ?? []).filter(changedValue);
    } catch (error) {
      // A partly saved batch is still one undo step for the saved photos.
      step.changes = (error as { culling?: CullingChange[] } | null)?.culling?.filter(changedValue) ?? [];
      throw error;
    } finally {
      useCatalogUndoStore.setState((s) =>
        s.past.includes(step)
          ? {
              past: step.changes.length ? [...s.past] : s.past.filter((item) => item !== step),
            }
          : {},
      );
    }
  });
}

function candidate(direction: 'undo' | 'redo'): 'catalog' | 'edit' | null {
  const state = useCatalogUndoStore.getState();
  if (state.busy) return null;
  const step = (direction === 'undo' ? state.past : state.future).at(-1);
  const e = useEditorStore.getState();
  const editor = useUIStore.getState().activeView === 'editor' && e.selectedImage?.isReady;
  if (editor && e.historyPending && direction === 'redo') return null;
  const hasEdit =
    editor &&
    (direction === 'undo'
      ? e.historyPending || e.historyIndex > 0
      : !e.historyPending && e.historyIndex < e.history.length - 1);
  if (!hasEdit) return step ? 'catalog' : null;
  if (!step) return 'edit';
  if (direction === 'undo' && e.historyPending) return 'edit';
  const order = editUndoOrder(e.history[e.historyIndex + (direction === 'redo' ? 1 : 0)]);
  return direction === 'undo' ? (step.order > order ? 'catalog' : 'edit') : step.order < order ? 'catalog' : 'edit';
}
export const catalogUndoAvailable = (direction: 'undo' | 'redo') => candidate(direction) === 'catalog';
export const canUndo = () => candidate('undo') !== null;
export const canRedo = () => candidate('redo') !== null;
export function getUndoLabel(): string {
  return candidate('undo') === 'catalog'
    ? `Undo ${useCatalogUndoStore.getState().past.at(-1)!.label}`
    : canUndo()
      ? 'Undo Edit'
      : 'Undo';
}
export function getRedoLabel(): string {
  return candidate('redo') === 'catalog'
    ? `Redo ${useCatalogUndoStore.getState().future.at(-1)!.label}`
    : canRedo()
      ? 'Redo Edit'
      : 'Redo';
}

/** Updates culling indicators only. In particular, Undo never selects a photo
 * or requests auto-advance, and labels preserve unrelated keyword tags. */
export function showCullingChanges(changes: CullingChange[]) {
  useLibraryStore.getState().setLibrary((state) => {
    let imageRatings = state.imageRatings;
    let imageFlags = state.imageFlags;
    const colors = new Map<string, string | null>();
    for (const { path, after } of changes) {
      if (after.kind === 'rating') {
        if (imageRatings === state.imageRatings) imageRatings = { ...imageRatings };
        imageRatings[path] = after.value;
      }
      if (after.kind === 'flag') {
        if (imageFlags === state.imageFlags) imageFlags = { ...imageFlags };
        imageFlags[path] = after.value;
      }
      if (after.kind === 'color') colors.set(path, after.value);
    }
    const imageList = colors.size
      ? state.imageList.map((image) => {
          if (!colors.has(image.path)) return image;
          const tags = (image.tags ?? []).filter((tag) => !tag.startsWith('color:'));
          const color = colors.get(image.path);
          if (color) tags.push(`color:${color}`);
          return { ...image, tags };
        })
      : state.imageList;
    return { imageRatings, imageFlags, imageList };
  });
}

async function travel(direction: 'undo' | 'redo', fallback?: () => void): Promise<void> {
  const target = candidate(direction);
  if (target === 'edit') {
    useEditorStore.getState()[direction]();
    return;
  }
  if (!target) {
    if (!useCatalogUndoStore.getState().busy) fallback?.();
    return;
  }
  const startedOnBranch = branch;
  const from = direction === 'undo' ? 'past' : 'future';
  const to = direction === 'undo' ? 'future' : 'past';
  const step = useCatalogUndoStore.getState()[from].at(-1)!;
  useCatalogUndoStore.setState({ busy: true });
  try {
    await enqueue(async () => {
      if (!step.changes.length || !useCatalogUndoStore.getState()[from].includes(step)) return;
      const values = step.changes.map(({ path, assetId, before, after }) => ({
        path,
        ...(assetId ? { assetId } : {}),
        value: direction === 'undo' ? before : after,
      }));
      try {
        const changed = await invokeInOrder<CullingChange[]>(Invokes.RestoreCulling, {
          values,
          directory: step.directory,
        });
        if (useLibraryStore.getState().sessionDirectory === step.directory) showCullingChanges(changed);
      } catch (error) {
        if (useLibraryStore.getState().sessionDirectory === step.directory)
          showCullingChanges((error as { culling?: CullingChange[] } | null)?.culling ?? []);
        throw error;
      }
      if (useCatalogUndoStore.getState()[from].includes(step))
        useCatalogUndoStore.setState((s) => ({
          [from]: s[from].filter((item) => item !== step),
          [to]: branch === startedOnBranch ? [...s[to], step] : s[to],
        }));
    });
  } catch (error) {
    // Keep the step for retry if a restore failed. Reapplying already saved
    // values is harmless; a partial restore must never lose its undo record.
    toast.error(`Failed to ${direction}: ${String((error as { message?: string })?.message ?? error)}`);
  } finally {
    useCatalogUndoStore.setState({ busy: false });
  }
}
export const undo = (fallback?: () => void) => travel('undo', fallback);
export const redo = (fallback?: () => void) => travel('redo', fallback);

onNewEdit(() => {
  branch++;
  useCatalogUndoStore.setState({ future: [] });
});
// Histories belong to one open library. A stale response cannot add steps to
// the next library; catalogAction retains the identity of its placeholder.
useLibraryStore.subscribe((s, previous) => {
  if (s.sessionDirectory !== previous.sessionDirectory) useCatalogUndoStore.setState({ past: [], future: [] });
});
