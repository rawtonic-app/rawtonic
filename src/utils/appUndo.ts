import i18n from "i18next";
import { createElement } from "react";
import { Invokes } from "../components/ui/AppProperties";
import { create } from "zustand";
import { toast } from "react-toastify";
import { debouncedSetHistory, useEditorStore } from "../store/useEditorStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { useUIStore } from "../store/useUIStore";
import { movedPath, type PathMove } from "./pathKeyedState";
import { invokeInOrder } from "./catalogWrites";
import { xmpMetadataRead } from "./catalogEvents";
import { photoWorkPending, reservePhotoWrites } from "./photoWrites";
import { editUndoOrder, nextUndoOrder, onNewEdit } from "./undoOrder";
import { registerWrittenSteps, travelKeptHistory } from "./photoHistory";

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
/** Top-level recipe values of one photo (`set_recipe_values`); `null`
 * removes a key. */
export interface RecipeValues {
  path: string;
  values: Record<string, unknown>;
}
interface RecipeChange {
  path: string;
  before: Record<string, unknown>;
  after: Record<string, unknown>;
}
/** One photo's whole recipe before and after a batch edit (paste, sync,
 * reset, styles, lens correction: `RecipeChange` in edits.rs). */
export interface RecipeSnapshot {
  path: string;
  before: unknown;
  after: unknown;
}
/** How a travelled recipe step reaches the open photograph: its editor
 * entry for the step gets `order`; with `skipOpen` its editor travelled the
 * step itself (a linked entry), so it is not loaded again. */
export interface RecipeShown {
  order: number;
  skipOpen: boolean;
  direction: "undo" | "redo";
}
/** Writes recipe values and returns each written photo's values before
 * (registered by useEditorActions, which knows the open photo's saves). */
type RecipeWriter = (
  values: RecipeValues[],
  shown?: RecipeShown,
) => Promise<RecipeValues[]>;
/** Writes whole recipes back; resolves with the photos written. */
type RecipeReplacer = (
  recipes: { path: string; adjustments: unknown }[],
  shown: RecipeShown,
) => Promise<string[]>;
let recipeWriter: RecipeWriter | null = null;
let recipeReplacer: RecipeReplacer | null = null;
export function registerRecipeWriter(
  writer: RecipeWriter,
  replacer?: RecipeReplacer,
) {
  recipeWriter = writer;
  recipeReplacer = replacer ?? null;
}
export type BatchLabel =
  | "matchLight"
  | "paste"
  | "sync"
  | "reset"
  | "style"
  | "lensCorrection"
  | "apply";
interface Step {
  order: number;
  action: CullingValue["kind"] | "recipes" | "xmp";
  changes: CullingChange[];
  /** A batch recipe change (series match): one step for all its photos. */
  recipes?: RecipeChange[];
  /** A batch edit of whole recipes (paste, sync, reset, …). */
  snapshots?: RecipeSnapshot[];
  label?: BatchLabel;
  /** Use XMP: the photos it was asked for and what undoes each reading. */
  xmp?: { paths: string[]; receipts: XmpReceipt[] };
  directory: string | null;
}
/** Opaque to the webview: what puts a photo's metadata back after Use XMP
 * (`XmpUndo` in rawtonic_catalog), handed back to `session_xmp_undo`. */
export type XmpReceipt = Record<string, unknown>;
/** What resolving XMP sidecars or undoing Use XMP did (`XmpReport` in
 * src-tauri/src/xmp_review.rs), by photo path. */
export interface XmpReport {
  done: string[];
  offline: string[];
  /** Changed again meanwhile: still to review. */
  conflicts: string[];
  failed: { path: string; reason: string }[];
  /** Use XMP: what undoes it. Its undo: the readings not undone. */
  undo: XmpReceipt[];
  /** Use XMP: the photos read, one per `undo` receipt. */
  read: string[];
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
const failureTokens = new WeakMap<Step, symbol>();
const failureNoticeId = (direction: "undo" | "redo", step: Step) =>
  `culling-${direction}-${step.order}`;
interface PendingWork {
  lane: "catalog" | "file";
  paths?: readonly string[];
  done: Promise<unknown>;
  /** A catalog step that has started: it gives its writes to the photos'
   * write queues (utils/photoWrites) before its first wait, so they order a
   * later batch write behind them. */
  submitted?: boolean;
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
  work: (job: PendingWork) => Promise<T>,
  lane: "catalog" | "file" = "file",
  paths?: readonly string[],
  /** Not waiting for a catalog step already started: the photos' write
   * queues order this one's writes behind its. One still waiting reserves
   * this one's photographs (a read of their recipes waits for it). */
  passSubmitted = false,
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
        (passSubmitted && job.submitted) ||
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
  const job: PendingWork = { lane, paths, done: Promise.resolve() };
  // Nothing to wait for: it starts at once, so its writes join the photos'
  // write queues before anything given after it (utils/photoWrites).
  const start = () => {
    const started = work(job);
    if (lane === "catalog") job.submitted = true;
    return started;
  };
  const run =
    dependencies.size > 0
      ? Promise.all([...dependencies].map((job) => job.done)).then(start)
      : new Promise<T>((resolve) => resolve(start()));
  job.done = run.catch(() => undefined);
  if (passSubmitted && dependencies.size > 0 && paths) reservePhotoWrites([...paths], run);
  pendingWork.add(job);
  void job.done.then(() => pendingWork.delete(job));
  // A catalog step waits for earlier work before it writes: an export,
  // quit or library switch waits for it too.
  if (lane === "catalog") photoWorkPending(run);
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
                resolved = resolved.map((path) => movedPath(path, move.moves));
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

/** Writes per-photo recipe values (series match) as one undoable step for
 * all photos written. Resolves with the photos written. */
export function recipeAction(values: RecipeValues[]): Promise<string[]> {
  debouncedSetHistory.flush();
  branch++;
  const editor = useEditorStore.getState();
  editor.setEditor({
    history: editor.history.slice(0, editor.historyIndex + 1),
  });
  const step: Step = {
    order: nextUndoOrder(),
    action: "recipes",
    changes: [],
    recipes: [],
    directory: useLibraryStore.getState().sessionDirectory,
  };
  const paths = values.map((value) => value.path);
  currentTargets.set(step, paths);
  useCatalogUndoStore.setState((s) => ({
    past: [...s.past, step].slice(-100),
    future: [],
    filesFuture: [],
    fileRedoBlocked: true,
  }));
  return enqueue(async () => {
    try {
      const before = recipeWriter
        ? await recipeWriter(values, {
            order: step.order,
            skipOpen: false,
            direction: "redo",
          })
        : [];
      step.recipes = before.map(({ path, values: old }) => ({
        path,
        before: old,
        after: values.find((value) => value.path === path)?.values ?? {},
      }));
      return step.recipes.map((change) => change.path);
    } finally {
      currentTargets.set(step, step.recipes!.map((change) => change.path));
      if (!step.recipes!.length)
        useCatalogUndoStore.setState((s) => ({
          past: s.past.filter((item) => item !== step),
        }));
    }
  }, "catalog", paths);
}

/** Runs a batch recipe edit of `paths` (paste, sync, reset, …) as one
 * undoable step for every photo it wrote, as Lightroom's Undo of a Sync.
 * `order`: the open photograph's own editor entry for the same edit (both
 * travel together). `work` submits its writes to the photographs' write
 * queues (tagged with the step's order) once earlier steps for them are done.
 * Resolves with the photos not written. */
export function recipeBatchAction(
  label: BatchLabel,
  work: (order: number) => Promise<{ failed: string[]; changes: RecipeSnapshot[] }>,
  paths: readonly string[],
  order?: number,
): Promise<string[]> {
  debouncedSetHistory.flush();
  branch++;
  // A new step branches: the open photograph's redo goes too.
  const editor = useEditorStore.getState();
  if (order === undefined && editor.historyIndex < editor.history.length - 1)
    editor.setEditor({
      history: editor.history.slice(0, editor.historyIndex + 1),
    });
  const step: Step = {
    order: order ?? nextUndoOrder(),
    action: "recipes",
    changes: [],
    snapshots: [],
    label,
    directory: useLibraryStore.getState().sessionDirectory,
  };
  currentTargets.set(step, [...paths]);
  useCatalogUndoStore.setState((s) => ({
    past: [...s.past, step].slice(-100),
    future: [],
    filesFuture: [],
    fileRedoBlocked: true,
  }));
  return enqueue(async () => {
    try {
      const { failed, changes } = await work(step.order);
      step.snapshots = changes;
      return failed;
    } finally {
      currentTargets.set(step, step.snapshots!.map((change) => change.path));
      // A step that wrote nothing but has its editor entry stays: the open
      // photograph's part travels with it.
      if (!step.snapshots!.length && order === undefined)
        useCatalogUndoStore.setState((s) => ({
          past: s.past.filter((item) => item !== step),
        }));
    }
  }, "catalog", paths, true);
}

/** Use XMP of the session bar's XMP status: the sidecars of `paths` that
 * another program changed are read into the library (rating, label, flag,
 * keywords, text and IPTC; never the edit) as one undo step for every photo
 * read. Its write joins the photos' write queues (`invokeInOrder`). Keep
 * library writes only sidecars, so it is no undo step. */
export function xmpReadAction(directory: string, paths: string[]): Promise<XmpReport> {
  debouncedSetHistory.flush();
  branch++;
  const editor = useEditorStore.getState();
  editor.setEditor({
    history: editor.history.slice(0, editor.historyIndex + 1),
  });
  const step: Step = {
    order: nextUndoOrder(),
    action: "xmp",
    changes: [],
    xmp: { paths: [...paths], receipts: [] },
    directory,
  };
  currentTargets.set(step, [...paths]);
  useCatalogUndoStore.setState((s) => ({
    past: [...s.past, step].slice(-100),
    future: [],
    filesFuture: [],
    fileRedoBlocked: true,
  }));
  return enqueue(async () => {
    try {
      const report = await readXmp(directory, paths);
      step.xmp!.receipts = report.undo;
      return report;
    } finally {
      if (!step.xmp!.receipts.length)
        useCatalogUndoStore.setState((s) => ({
          past: s.past.filter((item) => item !== step),
        }));
      xmpMetadataRead(directory);
    }
  }, "catalog", paths);
}
function readXmp(directory: string, paths: string[]) {
  return invokeInOrder<XmpReport>(
    Invokes.SessionXmpResolve,
    { directory, paths, keep: "xmp" },
    "useXmp",
  );
}
/** Why the photos of a report were not read or put back, for a notice. */
function xmpFailures(report: XmpReport) {
  return report.failed
    .map(({ path, reason }) => `${path.split(/[\\/]/).pop()}: ${reason}`)
    .join("; ");
}
/** Undo puts each photo's metadata back (unless it was edited since);
 * redo reads the sidecars again. Photos that travelled move to the other
 * side as one step; those that did not (edited since, offline, a failed
 * write) stay where they were, to try again, and a notice says why. */
async function travelXmp(
  step: Step,
  direction: "undo" | "redo",
  startedOnBranch: number,
) {
  const from = direction === "undo" ? "past" : "future";
  const to = direction === "undo" ? "future" : "past";
  const xmp = step.xmp!;
  const directory = step.directory ?? "";
  const part = (paths: string[], receipts: XmpReceipt[]): Step => {
    const split = { ...step, xmp: { paths, receipts } };
    currentTargets.set(split, paths);
    return split;
  };
  try {
    await enqueue(async () => {
      if (!useCatalogUndoStore.getState()[from].includes(step)) return;
      let moved: Step | null = null;
      let left: Step | null = null;
      let failure = "";
      try {
        if (direction === "undo") {
          const report = await invokeInOrder<XmpReport>(
            Invokes.SessionXmpUndo,
            { directory, steps: xmp.receipts, paths: xmp.paths },
            "useXmpUndo",
          );
          // Undone ones are redone by reading their sidecars again.
          if (report.done.length) moved = part(report.done, []);
          if (report.undo.length) {
            left = part(report.failed.map(({ path }) => path), report.undo);
            failure = xmpFailures(report);
          }
        } else {
          const report = await readXmp(directory, xmp.paths);
          if (report.read.length) moved = part(report.read, report.undo);
          // Offline or failed before reading: kept to redo. Photos whose
          // sidecar is no longer changed elsewhere have nothing to read.
          const unread = [
            ...report.offline,
            ...report.failed.map(({ path }) => path),
          ].filter((path) => !report.read.includes(path));
          if (unread.length) {
            left = part(unread, []);
            failure = [
              xmpFailures({ ...report, failed: report.failed.filter(({ path }) => !report.read.includes(path)) }),
              ...report.offline.map((path) => `${path.split(/[\\/]/).pop()}: ${i18n.t("session.xmp.reason.offline", { count: 1 })}`),
            ]
              .filter(Boolean)
              .join("; ");
          }
        }
      } finally {
        xmpMetadataRead(directory);
      }
      useCatalogUndoStore.setState((s) => ({
        [from]: s[from].flatMap((item) => (item === step ? (left ? [left] : []) : [item])),
        [to]: moved && branch === startedOnBranch ? [...s[to], moved] : s[to],
      }));
      if (failure) throw new Error(failure);
    }, "catalog", xmp.paths);
  } catch (error) {
    const detail = String((error as { message?: string })?.message ?? error);
    toast.error(
      direction === "undo"
        ? i18n.t("studio.library.fileOperations.undoFailed", { error: detail })
        : i18n.t("studio.library.fileOperations.redoFailed", { error: detail }),
    );
  }
}

/** The batch steps (still to undo) that wrote `path`, oldest first, with
 * the recipe each wrote there; `written`: leaving out those (that had
 * written it when its History was kept), undefined: none. */
export function recipeStepsSince(
  path: string,
  written: ReadonlySet<number> | undefined,
): { order: number; after: unknown }[] {
  if (written === undefined) return [];
  return useCatalogUndoStore.getState().past.flatMap((step) => {
    const change = step.snapshots?.find((item) => item.path === path);
    return !written.has(step.order) && change
      ? [{ order: step.order, after: change.after }]
      : [];
  });
}
registerWrittenSteps((path) =>
  useCatalogUndoStore
    .getState()
    .past.filter((step) => step.snapshots?.some((item) => item.path === path))
    .map((step) => step.order),
);

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
/** Whether the open photograph's next History entry in `direction` is its
 * own part of the batch recipe step `step` (the same undo order). */
function linkedEdit(step: Step, direction: "undo" | "redo") {
  if (!step.recipes && !step.snapshots) return false;
  const e = useEditorStore.getState();
  if (!e.selectedImage?.isReady) return false;
  const index = direction === "undo" ? e.historyIndex : e.historyIndex + 1;
  if (direction === "undo" ? index < 1 : index >= e.history.length) return false;
  return editUndoOrder(e.history, index) === step.order;
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
function batchLabel(label: BatchLabel | undefined) {
  switch (label) {
    case "paste":
      return i18n.t("undoHistory.paste");
    case "sync":
      return i18n.t("undoHistory.sync");
    case "reset":
      return i18n.t("undoHistory.reset");
    case "style":
      return i18n.t("undoHistory.style");
    case "lensCorrection":
      return i18n.t("undoHistory.lensCorrection");
    case "apply":
      return i18n.t("undoHistory.apply");
    default:
      return i18n.t("undoHistory.matchLight");
  }
}
function actionLabel(direction: "undo" | "redo") {
  const kind = candidate(direction);
  if (kind === "edit") {
    const step = catalogStep(direction);
    return step && linkedEdit(step, direction)
      ? batchLabel(step.label)
      : i18n.t("undoHistory.edit");
  }
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
    if (action === "recipes")
      return batchLabel(catalogStep(direction)!.label);
    if (action === "xmp") return i18n.t("undoHistory.useXmp");
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

function noticeText(action: string, changes: CullingChange[], limit = 1) {
  const name = changes
    .slice(0, limit)
    .map(({ path }) => path.split("?vc=")[0].split(/[\\/]/).pop())
    .join(", ");
  return changes.length > limit
    ? i18n.t("undoHistory.noticeMany", {
        action,
        name,
        remaining: changes.length - limit,
      })
    : i18n.t("undoHistory.notice", { action, name });
}
/** Viewers identify the shown photo; the grid uses its loaded folder list. */
function notifyOtherPhotos(action: string, changes: CullingChange[]) {
  const ui = useUIStore.getState();
  const library = useLibraryStore.getState();
  const shown = new Set(
    ui.activeView === "editor"
      ? [useEditorStore.getState().selectedImage?.path]
      : ui.libraryLoupe || ui.isFullScreen
        ? [library.libraryActivePath]
        : library.imageList.map((image) => image.path),
  );
  const offscreen = changes.filter(
    ({ path, after }) =>
      !shown.has(path) &&
      !(
        after.kind === "flag" &&
        [...shown].some(
          (visible) => visible?.split("?vc=")[0] === path.split("?vc=")[0],
        )
      ),
  );
  if (offscreen.length)
    toast.info(noticeText(action, offscreen), { autoClose: 3000 });
}
function sameCullingTarget(left: CullingChange, right: CullingChange) {
  if (left.before.kind !== right.before.kind) return false;
  if (left.assetId && right.assetId)
    return (
      left.assetId === right.assetId &&
      left.directory === right.directory &&
      left.path.split("?vc=")[1] === right.path.split("?vc=")[1]
    );
  return left.path === right.path;
}
/** A partial restore travels only its acknowledged targets. Failed targets
 * remain in place, so retry cannot undo anything older or lose saved redo. */
function splitRestored(
  step: Step,
  restored: CullingChange[],
  direction: "undo" | "redo",
  startedOnBranch: number,
) {
  const done = step.changes.filter((change) =>
    restored.some((receipt) => sameCullingTarget(change, receipt)),
  );
  if (!done.length) return step;
  failureTokens.delete(step);
  const remaining = step.changes.filter((change) => !done.includes(change));
  const failed = remaining.length ? { ...step, changes: remaining } : null;
  const saved = { ...step, changes: done };
  const paths = currentTargets.get(step);
  if (paths) {
    if (failed)
      currentTargets.set(
        failed,
        paths.filter((_, index) => !done.includes(step.changes[index])),
      );
    currentTargets.set(
      saved,
      paths.filter((_, index) => done.includes(step.changes[index])),
    );
  }
  const from = direction === "undo" ? "past" : "future";
  const to = direction === "undo" ? "future" : "past";
  useCatalogUndoStore.setState((state) => ({
    [from]: state[from].flatMap((item) =>
      item === step ? (failed ? [failed] : []) : [item],
    ),
    [to]: branch === startedOnBranch ? [...state[to], saved] : state[to],
  }));
  return failed;
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
  const top = catalogStep(direction);
  // One step for every photo of a batch edit: the open photograph's own
  // entry and the others' recipes travel together.
  const linked =
    (target === "edit" || target === "catalog") &&
    !!top &&
    linkedEdit(top, direction);
  if (target === "edit" && !linked) {
    useEditorStore.getState()[direction]();
    return;
  }
  if (linked) {
    useEditorStore.getState()[direction]();
    useCatalogUndoStore.setState({ busy: true });
    try {
      await travelRecipes(top, direction, branch, true);
    } finally {
      useCatalogUndoStore.setState({ busy: false });
    }
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
  if (step.xmp) {
    try {
      await travelXmp(step, direction, startedOnBranch);
    } finally {
      useCatalogUndoStore.setState({ busy: false });
    }
    return;
  }
  if (step.recipes || step.snapshots) {
    try {
      await travelRecipes(step, direction, startedOnBranch);
    } finally {
      useCatalogUndoStore.setState({ busy: false });
    }
    return;
  }
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
          notifyOtherPhotos(label, changed);
        }
      } catch (error) {
        if (useLibraryStore.getState().sessionDirectory === step.directory) {
          const restored =
            (error as { culling?: CullingChange[] } | null)?.culling ?? [];
          showCullingChanges(restored);
          notifyOtherPhotos(label, restored);
        }
        throw error;
      }
      failureTokens.delete(step);
      toast.dismiss(failureNoticeId(direction, step));
      if (useCatalogUndoStore.getState()[from].includes(step))
        useCatalogUndoStore.setState((s) => ({
          [from]: s[from].filter((item) => item !== step),
          [to]: branch === startedOnBranch ? [...s[to], step] : s[to],
        }));
    }, "catalog", currentTargets.get(step));
  } catch (error) {
    if (!useCatalogUndoStore.getState()[from].includes(step)) return;
    const failed = splitRestored(
      step,
      (error as { culling?: CullingChange[] } | null)?.culling ?? [],
      direction,
      startedOnBranch,
    );
    const detail = String((error as { message?: string })?.message ?? error);
    const message =
      direction === "undo"
        ? i18n.t("studio.library.fileOperations.undoFailed", { error: detail })
        : i18n.t("studio.library.fileOperations.redoFailed", { error: detail });
    if (!failed) {
      toast.error(message);
      return;
    }
    const failure = Symbol();
    failureTokens.set(failed, failure);
    const toastId = failureNoticeId(direction, failed);
    const content = ({ closeToast }: { closeToast: () => void }) =>
      createElement(
        "div",
        { className: "space-y-2" },
        createElement("p", null, noticeText(message, failed.changes, 3)),
        createElement(
          "button",
          {
            type: "button",
            className: "underline",
            onClick: () => {
              const state = useCatalogUndoStore.getState();
              if (state.busy) return;
              // Only this explicit click discards the still-failed targets. A
              // stale notice cannot discard a retried step or another library.
              if (
                state[from].includes(failed) &&
                failureTokens.get(failed) === failure
              ) {
                useCatalogUndoStore.setState({
                  [from]: state[from].filter((item) => item !== failed),
                });
                failureTokens.delete(failed);
              }
              closeToast();
            },
          },
          i18n.t("undoHistory.skipFailed"),
        ),
      );
    if (toast.isActive(toastId))
      toast.update(toastId, { render: content, autoClose: false });
    else toast.error(content, { toastId, autoClose: false });
  } finally {
    useCatalogUndoStore.setState({ busy: false });
  }
}
/** Undo/redo of a recipe step: writes every photo's values back (or again).
 * Writing is idempotent, so a partly failed step stays in place to retry. */
async function travelRecipes(
  step: Step,
  direction: "undo" | "redo",
  startedOnBranch: number,
  skipOpen = false,
) {
  const from = direction === "undo" ? "past" : "future";
  const to = direction === "undo" ? "future" : "past";
  const shown = { order: step.order, skipOpen, direction };
  try {
    await enqueue(async () => {
      if (!useCatalogUndoStore.getState()[from].includes(step)) return;
      let missing: boolean;
      if (step.snapshots) {
        const recipes = step.snapshots.map(({ path, before, after }) => ({
          path,
          adjustments: direction === "undo" ? before : after,
        }));
        if (recipes.length) {
          const written = recipeReplacer
            ? await recipeReplacer(recipes, shown)
            : [];
          missing = written.length < recipes.length;
        } else missing = false;
      } else {
        const values = (step.recipes ?? []).map(({ path, before, after }) => ({
          path,
          values: direction === "undo" ? before : after,
        }));
        if (!values.length) return;
        const written = recipeWriter ? await recipeWriter(values, shown) : [];
        missing = written.length < values.length;
      }
      // Photographs not open keep a History that follows the step.
      const open = useEditorStore.getState().selectedImage?.path;
      for (const { path } of step.snapshots ?? step.recipes ?? [])
        if (path !== open) travelKeptHistory(path, step.order, direction);
      if (missing) throw new Error(i18n.t("undoHistory.recipesNotAll"));
      if (useCatalogUndoStore.getState()[from].includes(step))
        useCatalogUndoStore.setState((s) => ({
          [from]: s[from].filter((item) => item !== step),
          [to]: branch === startedOnBranch ? [...s[to], step] : s[to],
        }));
    }, "catalog", currentTargets.get(step));
  } catch (error) {
    const detail = String((error as { message?: string })?.message ?? error);
    toast.error(
      direction === "undo"
        ? i18n.t("studio.library.fileOperations.undoFailed", { error: detail })
        : i18n.t("studio.library.fileOperations.redoFailed", { error: detail }),
    );
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
