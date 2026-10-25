import i18n from "i18next";
import { Invokes } from "../components/ui/AppProperties";
import { create } from "zustand";
import { toast } from "react-toastify";
import { debouncedSetHistory, useEditorStore } from "../store/useEditorStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { useUIStore } from "../store/useUIStore";
import { movedPath, type PathMove } from "./pathKeyedState";
import { invokeInOrder } from "./catalogWrites";
import { editUndoOrder, nextUndoOrder, onNewEdit } from "./undoOrder";

export type CullingValue =
  | { kind: "rating" | "flag"; value: number }
  | { kind: "color"; value: string | null };
export interface CullingChange {
  path: string;
  assetId?: string | null;
  directory?: string | null;
  before: CullingValue;
  after: CullingValue;
}
const changedValue = ({ before, after }: CullingChange) =>
  before.kind !== after.kind || before.value !== after.value;
interface Step {
  order: number;
  action: CullingValue["kind"];
  changes: CullingChange[];
  directory: string | null;
}
interface FileStep {
  order: number;
  kind: string;
  id?: string;
}
interface FileSummary {
  id: string;
  kind?: string;
  moves?: PathMove[];
}
interface FileHandlers {
  undo(): void | Promise<void>;
  redo(): void | Promise<void>;
}
let fileHandlers: FileHandlers | null = null;
interface CatalogUndoState {
  past: Step[];
  future: Step[];
  busy: boolean;
  filesPast: FileStep[];
  filesFuture: FileStep[];
  /** New branches suppress unknown native redo entries; newly undone tracked files remain eligible. */
  fileRedoBlocked: boolean;
  fileUndoEmpty: boolean;
}
export const useCatalogUndoStore = create<CatalogUndoState>(() => ({
  past: [],
  future: [],
  busy: false,
  filesPast: [],
  filesFuture: [],
  fileRedoBlocked: false,
  fileUndoEmpty: false,
}));
const currentTargets = new WeakMap<Step, readonly string[]>();
interface PendingWork {
  lane: "catalog" | "file";
  paths?: readonly string[];
  done: Promise<unknown>;
}
const pendingWork = new Set<PendingWork>();
function overlaps(left?: readonly string[], right?: readonly string[]) {
  if (!left || !right) return true; // Unknown journal targets require serialization.
  const physical = (path: string) =>
    path.split("?vc=")[0].replaceAll("\\", "/").replace(/\/$/, "");
  return left.some((a) =>
    right.some((b) => {
      const x = physical(a),
        y = physical(b);
      return x === y || x.startsWith(`${y}/`) || y.startsWith(`${x}/`);
    }),
  );
}
let branch = 0;
let fileTravelBranch: number | null = null;
const pendingCatalog = new Set<number>();
let completedMoves: { order: number; moves: Map<string, string> }[] = [];
export interface CatalogActionContext {
  resolvePaths(paths: readonly string[]): string[];
}
function recordMoves(moves: PathMove[]) {
  if (!moves.length) return;
  const mapped = new Map<string, string>();
  for (const { from, to } of moves) {
    const source = from.split("?vc=")[0];
    const destination = to.split("?vc=")[0];
    if (source !== destination) mapped.set(source, destination);
  }
  if (!mapped.size) return;
  // Receipts keep their native identity; dependency targets follow later moves.
  const remap = (paths: readonly string[]) =>
    paths.map((path) => movedPath(path, mapped));
  const { past, future } = useCatalogUndoStore.getState();
  for (const step of [...past, ...future]) {
    const paths = currentTargets.get(step);
    if (paths) currentTargets.set(step, remap(paths));
  }
  for (const job of pendingWork) if (job.paths) job.paths = remap(job.paths);
  if (pendingCatalog.size)
    completedMoves.push({ order: nextUndoOrder(), moves: mapped });
}
function pruneMoves() {
  let oldest = Infinity;
  for (const order of pendingCatalog) oldest = Math.min(oldest, order);
  completedMoves = completedMoves.filter((move) => move.order > oldest);
}

function enqueue<T>(
  work: () => Promise<T>,
  lane: "catalog" | "file" = "file",
  paths?: readonly string[],
): Promise<T> {
  // Writes to the same paths stay in input order. Independent metadata can
  // pass a long move; file commands remain serialized with prior work.
  const dependencies = new Set<PendingWork>();
  let affected = paths;
  // A→B followed by B→C also conflicts with a write still naming A. Only
  // earlier jobs participate, so later files waiting for this write cannot cycle.
  let added: boolean;
  do {
    added = false;
    for (const job of pendingWork) {
      if (
        dependencies.has(job) ||
        (lane !== "file" && !overlaps(affected, job.paths))
      )
        continue;
      dependencies.add(job);
      if (job.lane === "file")
        affected =
          affected && job.paths ? [...affected, ...job.paths] : undefined;
      added = true;
    }
  } while (added);
  const run = Promise.all([...dependencies].map((job) => job.done)).then(work);
  const job: PendingWork = { lane, paths, done: run.catch(() => undefined) };
  pendingWork.add(job);
  void job.done.then(() => pendingWork.delete(job));
  return run;
}

/** Start one catalog step at input time, before waiting for the disk. Native
 * receipts name the actual old values and only the photos committed. */
export function catalogAction(
  action: CullingValue["kind"],
  work: (context: CatalogActionContext) => Promise<CullingChange[]>,
  paths?: readonly string[],
): Promise<void> {
  debouncedSetHistory.flush();
  branch++;
  const editor = useEditorStore.getState();
  // A catalog edit branches the same way as a Develop edit.
  editor.setEditor({
    history: editor.history.slice(0, editor.historyIndex + 1),
  });
  const step: Step = {
    order: nextUndoOrder(),
    action,
    changes: [],
    directory: useLibraryStore.getState().sessionDirectory,
  };
  if (paths) currentTargets.set(step, [...paths]);
  pendingCatalog.add(step.order);
  useCatalogUndoStore.setState((s) => ({
    past: [...s.past, step].slice(-100),
    future: [],
    filesFuture: [],
    fileRedoBlocked: true,
  }));
  return enqueue(async () => {
    try {
      step.changes = (
        (await work({
          resolvePaths: (paths) => {
            let resolved = [...paths];
            for (const move of completedMoves)
              if (move.order > step.order)
                resolved = resolved.map((path) =>
                  movedPath(path, move.moves),
                );
            return resolved;
          },
        })) ?? []
      ).filter(changedValue);
    } catch (error) {
      // A partly saved batch is still one undo step for the saved photos.
      step.changes =
        (error as { culling?: CullingChange[] } | null)?.culling?.filter(
          changedValue,
        ) ?? [];
      throw error;
    } finally {
      currentTargets.set(
        step,
        step.changes.map((change) => change.path),
      );
      pendingCatalog.delete(step.order);
      pruneMoves();
      useCatalogUndoStore.setState((s) =>
        s.past.includes(step)
          ? {
              past: step.changes.length
                ? [...s.past]
                : s.past.filter((item) => item !== step),
            }
          : {},
      );
    }
  }, "catalog", paths);
}

/** The file journal stays native; these small markers order its successful
 * commands with metadata in this window. Native history predating the window
 * is reached only after the tracked newer steps. */
export function registerFileUndoHandlers(handlers: FileHandlers) {
  fileHandlers = handlers;
  useCatalogUndoStore.setState({});
  return () => {
    if (fileHandlers === handlers) fileHandlers = null;
  };
}
export const undoBranchId = () => fileTravelBranch ?? branch;
export function fileOperationTravel(
  direction: "undo" | "redo",
  summary: FileSummary | null | undefined,
  startedOnBranch = branch,
) {
  recordMoves(summary?.moves ?? []);
  const from = direction === "undo" ? "filesPast" : "filesFuture";
  const to = direction === "undo" ? "filesFuture" : "filesPast";
  useCatalogUndoStore.setState((s) => {
    if (!summary)
      return {
        [from]: [],
        ...(direction === "undo"
          ? { fileUndoEmpty: true }
          : { fileRedoBlocked: true }),
      };
    const found = s[from].at(-1);
    const step: FileStep =
      found && (!found.id || found.id === summary.id)
        ? { ...found, id: summary.id, kind: summary.kind ?? found.kind }
        : { order: 0, id: summary.id, kind: summary.kind ?? "file" };
    return {
      [from]: found ? s[from].slice(0, -1) : [],
      [to]: startedOnBranch === branch ? [...s[to], step] : s[to],
      past: [...s.past],
      future: [...s.future],
      fileUndoEmpty: false,
      fileRedoBlocked: s.fileRedoBlocked,
    };
  });
}
export function clearFileUndoHistory() {
  useCatalogUndoStore.setState({
    filesPast: [],
    filesFuture: [],
    fileUndoEmpty: true,
    fileRedoBlocked: true,
  });
}
export function fileAction<T>(
  work: () => Promise<T>,
  kind = "file",
  didChange?: (result: T) => boolean,
  movesForResult?: (result: T) => PathMove[],
  paths?: readonly string[],
): Promise<T> {
  debouncedSetHistory.flush();
  branch++;
  const actionBranch = branch;
  const editor = useEditorStore.getState();
  if (editor.historyIndex < editor.history.length - 1)
    editor.setEditor({
      history: editor.history.slice(0, editor.historyIndex + 1),
    });
  const step: FileStep = { order: nextUndoOrder(), kind };
  useCatalogUndoStore.setState((s) => ({
    filesPast: [...s.filesPast, step],
    filesFuture: [],
    future: [],
    fileRedoBlocked: true,
    fileUndoEmpty: false,
  }));
  return enqueue(async () => {
    let committed = false;
    try {
      const result = await work();
      committed = didChange
        ? didChange(result)
        : Array.isArray(result)
          ? result.length > 0
          : !!result && (result as { items?: number }).items !== 0;
      const summary = result as FileSummary | null;
      if (summary && !Array.isArray(summary)) {
        step.id = summary.id;
        step.kind = summary.kind ?? kind;
      }
      if (committed)
        recordMoves(
          movesForResult ? movesForResult(result) : (summary?.moves ?? []),
        );
      return result;
    } finally {
      useCatalogUndoStore.setState((s) =>
        !s.filesPast.includes(step)
          ? {}
          : {
              filesPast: committed
                ? [...s.filesPast]
                : s.filesPast.filter((item) => item !== step),
              ...(committed
                ? { fileRedoBlocked: actionBranch !== branch }
                : {}),
            },
      );
    }
  }, "file", paths);
}
function catalogStep(direction: "undo" | "redo") {
  return (
    direction === "undo"
      ? useCatalogUndoStore.getState().past
      : useCatalogUndoStore.getState().future
  ).at(-1);
}
function fileStep(direction: "undo" | "redo") {
  return (
    direction === "undo"
      ? useCatalogUndoStore.getState().filesPast
      : useCatalogUndoStore.getState().filesFuture
  ).at(-1);
}
function candidate(
  direction: "undo" | "redo",
): "catalog" | "edit" | "file" | null {
  const state = useCatalogUndoStore.getState();
  if (state.busy) return null;
  const step = catalogStep(direction);
  if (useUIStore.getState().activeView !== "editor") {
    const file = fileStep(direction);
    const fileAvailable =
      direction === "undo"
        ? !!file || (!!fileHandlers && !state.fileUndoEmpty)
        : !!file || (!state.fileRedoBlocked && !!fileHandlers);
    if (!step) return fileAvailable ? "file" : null;
    if (!fileAvailable) return "catalog";
    if (!file) return "catalog"; // Native operations from before this window are older.
    return direction === "undo"
      ? file.order > step.order
        ? "file"
        : "catalog"
      : file.order < step.order
        ? "file"
        : "catalog";
  }
  const e = useEditorStore.getState();
  if (e.selectedImage?.isReady && e.historyPending && direction === "redo")
    return null;
  const hasEdit =
    e.selectedImage?.isReady &&
    (direction === "undo"
      ? e.historyPending || e.historyIndex > 0
      : !e.historyPending && e.historyIndex < e.history.length - 1);
  if (!hasEdit) return step ? "catalog" : null;
  if (!step) return "edit";
  if (direction === "undo" && e.historyPending) return "edit";
  const order = editUndoOrder(
    e.history,
    e.historyIndex + (direction === "redo" ? 1 : 0),
  );
  return direction === "undo"
    ? step.order > order
      ? "catalog"
      : "edit"
    : step.order < order
      ? "catalog"
      : "edit";
}
export const catalogUndoAvailable = (direction: "undo" | "redo") =>
  candidate(direction) === "catalog";
export const canUndo = () => candidate("undo") !== null;
export const canRedo = () => candidate("redo") !== null;
function actionLabel(direction: "undo" | "redo") {
  const kind = candidate(direction);
  if (kind === "edit") return i18n.t("undoHistory.edit");
  if (kind === "file") {
    const action = fileStep(direction)?.kind.toLowerCase() ?? "";
    if (action.includes("move")) return i18n.t("undoHistory.move");
    if (action.includes("rename")) return i18n.t("undoHistory.rename");
    if (action.includes("trash")) return i18n.t("undoHistory.trash");
    return i18n.t("undoHistory.file");
  }
  if (kind === "catalog") {
    const action = catalogStep(direction)!.action;
    if (action === "rating") return i18n.t("undoHistory.rating");
    if (action === "flag") return i18n.t("undoHistory.flag");
    if (action === "color") return i18n.t("undoHistory.color");
    return null;
  }
  return null;
}
export function getUndoLabel(): string {
  const action = actionLabel("undo");
  return action
    ? i18n.t("undoHistory.undoAction", { action })
    : i18n.t("contextMenus.editor.undo");
}
export function getRedoLabel(): string {
  const action = actionLabel("redo");
  return action
    ? i18n.t("undoHistory.redoAction", { action })
    : i18n.t("contextMenus.editor.redo");
}

/** Updates culling indicators only. In particular, Undo never selects a photo
 * or requests auto-advance, and labels preserve unrelated keyword tags. */
export function showCullingChanges(changes: CullingChange[]) {
  useLibraryStore.getState().setLibrary((state) => {
    let imageRatings = state.imageRatings;
    let imageFlags = state.imageFlags;
    const colors = new Map<string, string | null>();
    for (const { path, after } of changes) {
      if (after.kind === "rating") {
        if (imageRatings === state.imageRatings)
          imageRatings = { ...imageRatings };
        imageRatings[path] = after.value;
      }
      if (after.kind === "flag") {
        if (imageFlags === state.imageFlags) imageFlags = { ...imageFlags };
        imageFlags[path] = after.value;
      }
      if (after.kind === "color") colors.set(path, after.value);
    }
    const imageList = colors.size
      ? state.imageList.map((image) => {
          if (!colors.has(image.path)) return image;
          const tags = (image.tags ?? []).filter(
            (tag) => !tag.startsWith("color:"),
          );
          const color = colors.get(image.path);
          if (color) tags.push(`color:${color}`);
          return { ...image, tags };
        })
      : state.imageList;
    return { imageRatings, imageFlags, imageList };
  });
}

async function travel(
  direction: "undo" | "redo",
  fallback?: () => void | Promise<void>,
): Promise<void> {
  // Settle an older draft before capturing the undo branch, also in Library.
  debouncedSetHistory.flush();
  const target = candidate(direction);
  if (target === "edit") {
    useEditorStore.getState()[direction]();
    return;
  }
  const inLibrary = useUIStore.getState().activeView !== "editor";
  const handler = inLibrary
    ? (fallback ?? fileHandlers?.[direction])
    : undefined;
  if (!target) {
    if (
      !useCatalogUndoStore.getState().busy &&
      !(direction === "redo" && useCatalogUndoStore.getState().fileRedoBlocked)
    )
      await handler?.();
    return;
  }
  if (target === "file") {
    if (!handler) return;
    const step = fileStep(direction);
    const requestedOnBranch = branch;
    const from = direction === "undo" ? "filesPast" : "filesFuture";
    useCatalogUndoStore.setState({ busy: true });
    try {
      await enqueue(async () => {
        if (!step || useCatalogUndoStore.getState()[from].includes(step)) {
          const previous = fileTravelBranch;
          fileTravelBranch = requestedOnBranch;
          try {
            await handler();
          } finally {
            fileTravelBranch = previous;
          }
        }
      });
    } finally {
      useCatalogUndoStore.setState({ busy: false });
    }
    return;
  }
  const startedOnBranch = branch;
  const from = direction === "undo" ? "past" : "future";
  const to = direction === "undo" ? "future" : "past";
  const step = useCatalogUndoStore.getState()[from].at(-1)!;
  const label = direction === "undo" ? getUndoLabel() : getRedoLabel();
  useCatalogUndoStore.setState({ busy: true });
  try {
    await enqueue(async () => {
      if (
        !step.changes.length ||
        !useCatalogUndoStore.getState()[from].includes(step)
      )
        return;
      const values = step.changes.map(
        ({ path, assetId, directory, before, after }) => ({
          path,
          ...(assetId ? { assetId, directory } : {}),
          value: direction === "undo" ? before : after,
        }),
      );
      try {
        const changed = await invokeInOrder<CullingChange[]>(
          Invokes.RestoreCulling,
          {
            values,
            directory: step.directory,
          },
        );
        if (useLibraryStore.getState().sessionDirectory === step.directory) {
          showCullingChanges(changed);
          const name = changed[0]?.path.split("?vc=")[0].split(/[\\/]/).pop();
          const more = changed.length > 1 ? ` (+${changed.length - 1})` : "";
          if (name)
            toast.info(`${label}: ${name}${more}`, { autoClose: 3000 });
        }
      } catch (error) {
        if (useLibraryStore.getState().sessionDirectory === step.directory)
          showCullingChanges(
            (error as { culling?: CullingChange[] } | null)?.culling ?? [],
          );
        throw error;
      }
      if (useCatalogUndoStore.getState()[from].includes(step))
        useCatalogUndoStore.setState((s) => ({
          [from]: s[from].filter((item) => item !== step),
          [to]: branch === startedOnBranch ? [...s[to], step] : s[to],
        }));
    }, "catalog", currentTargets.get(step));
  } catch (error) {
    // Keep the step for retry if a restore failed. Reapplying already saved
    // values is harmless; a partial restore must never lose its undo record.
    const message = String((error as { message?: string })?.message ?? error);
    toast.error(
      direction === "undo"
        ? i18n.t("studio.library.fileOperations.undoFailed", { error: message })
        : i18n.t("studio.library.fileOperations.redoFailed", {
            error: message,
          }),
    );
  } finally {
    useCatalogUndoStore.setState({ busy: false });
  }
}
export const undo = (fallback?: () => void | Promise<void>) =>
  travel("undo", fallback);
export const redo = (fallback?: () => void | Promise<void>) =>
  travel("redo", fallback);

onNewEdit(() => {
  branch++;
  useCatalogUndoStore.setState({
    future: [],
    filesFuture: [],
    fileRedoBlocked: true,
  });
});
// Histories belong to one open library. A stale response cannot add steps to
// the next library; catalogAction retains the identity of its placeholder.
useLibraryStore.subscribe((s, previous) => {
  if (s.sessionDirectory !== previous.sessionDirectory)
    useCatalogUndoStore.setState({
      past: [],
      future: [],
      filesPast: [],
      filesFuture: [],
      fileRedoBlocked: false,
      fileUndoEmpty: false,
    });
});
