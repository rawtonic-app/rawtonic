import '../i18n';
import { fireEvent, render } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { notify as toast } from "../activity/notify";
import { invoke } from "@tauri-apps/api/core";
import { debouncedSetHistory, useEditorStore } from "../store/useEditorStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { useUIStore } from "../store/useUIStore";
import { INITIAL_ADJUSTMENTS } from "./adjustments";
import { editUndoOrder } from "./undoOrder";
import {
  catalogAction,
  recipeAction,
  recipeBatchAction,
  registerRecipeWriter,
  type RecipeShown,
  type RecipeValues,
  undo,
  redo,
  canRedo,
  canUndo,
  getUndoLabel,
  useCatalogUndoStore,
} from "./appUndo";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
vi.mock("../activity/notify", () => ({
  notify: {
    error: vi.fn(),
    info: vi.fn(),
    dismiss: vi.fn(),
    update: vi.fn(),
    isActive: vi.fn(() => false),
  },
}));
const changes = [
  {
    path: "/a.nef",
    before: { kind: "rating" as const, value: 0 },
    after: { kind: "rating" as const, value: 4 },
  },
];
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(invoke).mockImplementation(async (_command, args) =>
    (
      (args as { values: { path: string; value: unknown }[] })?.values ?? []
    ).map(({ path, value }) => ({
      path,
      before: value,
      after: value,
    })),
  );
  debouncedSetHistory.cancel();
  useEditorStore.setState(useEditorStore.getInitialState());
  useEditorStore.setState({
    selectedImage: { path: "/a.nef", isReady: true } as never,
  });
  useLibraryStore.setState({
    sessionDirectory: "/job",
    imageRatings: {},
    imageFlags: {},
    imageList: [],
  });
  useUIStore.setState({ activeView: "editor" });
  useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState());
});
it("orders rate → edit → undo → undo, then redoes in the same order", async () => {
  await catalogAction("rating", async () => changes);
  const next = { ...INITIAL_ADJUSTMENTS, exposure: 1 };
  useEditorStore.getState().setEditor({ adjustments: next });
  debouncedSetHistory(next);
  expect(getUndoLabel()).toBe("Undo Edit");
  await undo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
  expect(getUndoLabel()).toBe("Undo Set Rating");
  await undo();
  expect(invoke).toHaveBeenLastCalledWith("restore_culling", {
    values: [{ path: "/a.nef", value: changes[0].before }],
    directory: "/job",
  });
  await redo();
  await redo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(1);
});
it("undo after auto-advance changes the rated photo without moving the current view", async () => {
  await catalogAction("rating", async () => changes);
  useEditorStore
    .getState()
    .setEditor({ selectedImage: { path: "/b.nef", isReady: true } as never });
  useEditorStore.getState().resetHistory(INITIAL_ADJUSTMENTS);
  useLibraryStore.setState({
    multiSelectedPaths: ["/b.nef"],
    libraryActivePath: "/b.nef",
  });
  await undo();
  expect(useEditorStore.getState().selectedImage?.path).toBe("/b.nef");
  expect(useLibraryStore.getState().multiSelectedPaths).toEqual(["/b.nef"]);
  expect(invoke).toHaveBeenLastCalledWith("restore_culling", expect.anything());
});
it("keeps pending writes in input order and waits for them before undo", async () => {
  let finish!: (value: typeof changes) => void;
  const pending = catalogAction(
    "rating",
    () =>
      new Promise<typeof changes>((resolve) => {
        finish = resolve;
      }),
  );
  const undone = undo();
  expect(invoke).not.toHaveBeenCalled();
  await Promise.resolve();
  finish(changes);
  await pending;
  await undone;
  expect(invoke).toHaveBeenCalledWith("restore_culling", expect.anything());
});
it("a new edit discards metadata redo", async () => {
  await catalogAction("rating", async () => changes);
  await undo();
  useEditorStore
    .getState()
    .pushHistory({ ...INITIAL_ADJUSTMENTS, exposure: 2 });
  expect(useCatalogUndoStore.getState().future).toEqual([]);
});

it("restores mixed ratings and no colour label without replacing keywords", async () => {
  const batch = [
    changes[0],
    {
      path: "/b.nef",
      before: { kind: "rating" as const, value: 2 },
      after: { kind: "rating" as const, value: 4 },
    },
  ];
  useLibraryStore.setState({ imageRatings: { "/a.nef": 4, "/b.nef": 4 } });
  await catalogAction("rating", async () => batch);
  await undo();
  expect(useLibraryStore.getState().imageRatings).toEqual({
    "/a.nef": 0,
    "/b.nef": 2,
  });
  useLibraryStore.setState({
    imageList: [{ path: "/a.nef", tags: ["user:beach", "color:red"] }] as never,
  });
  await catalogAction("color", async () => [
    {
      path: "/a.nef",
      before: { kind: "color", value: null },
      after: { kind: "color", value: "red" },
    },
  ]);
  await undo();
  expect(useLibraryStore.getState().imageList[0].tags).toEqual(["user:beach"]);
});
it("a partial action records only saved photos, and a partial undo remains retryable", async () => {
  await catalogAction("rating", async () => {
    throw { culling: changes, failed: ["/b.nef"] };
  }).catch(() => {});
  vi.mocked(invoke).mockRejectedValueOnce({ message: "offline", culling: [] });
  await undo();
  expect(getUndoLabel()).toBe("Undo Set Rating");
  await undo();
  expect(useCatalogUndoStore.getState().past).toHaveLength(0);
  expect(useCatalogUndoStore.getState().future[0].changes).toEqual(changes);
});
it("does not resurrect old history or indicators when a library closes during undo", async () => {
  await catalogAction("rating", async () => changes);
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const undone = undo();
  // app queue, catalog write queue, native call
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  useLibraryStore.setState({ sessionDirectory: "/another", imageRatings: {} });
  finish([{ ...changes[0], after: changes[0].before }]);
  await undone;
  expect(useCatalogUndoStore.getState().future).toEqual([]);
  expect(useLibraryStore.getState().imageRatings).toEqual({});
});

it("an edit made while catalog undo waits starts a new branch without reviving metadata redo", async () => {
  await catalogAction("rating", async () => changes);
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const undone = undo();
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  useEditorStore
    .getState()
    .pushHistory({ ...INITIAL_ADJUSTMENTS, exposure: 2 });
  finish([{ ...changes[0], after: changes[0].before }]);
  await undone;
  expect(useCatalogUndoStore.getState().future).toEqual([]);
});
it("sends stable asset identity back to the native restore helper", async () => {
  await catalogAction("rating", async () => [
    { ...changes[0], assetId: "asset-a", directory: "/owner" },
  ]);
  await undo();
  expect(invoke).toHaveBeenCalledWith("restore_culling", {
    directory: "/job",
    values: [
      {
        path: "/a.nef",
        assetId: "asset-a",
        directory: "/owner",
        value: changes[0].before,
      },
    ],
  });
});

it("a pending edit cannot redo the catalog branch it replaces", async () => {
  await catalogAction("rating", async () => changes);
  await undo();
  vi.mocked(invoke).mockClear();
  const next = { ...INITIAL_ADJUSTMENTS, exposure: 3 };
  useEditorStore.getState().setEditor({ adjustments: next });
  debouncedSetHistory(next);
  expect(canRedo()).toBe(false);
  await redo();
  expect(invoke).not.toHaveBeenCalled();
  debouncedSetHistory.flush();
  expect(useCatalogUndoStore.getState().future).toEqual([]);
});

it("orders a return to an evicted history entry ahead of catalog undo", async () => {
  for (let exposure = 1; exposure < 50; exposure++) {
    useEditorStore.getState().pushHistory({ ...INITIAL_ADJUSTMENTS, exposure });
  }
  await catalogAction("rating", async () => changes);
  const pending = { ...INITIAL_ADJUSTMENTS, exposure: 50 };
  useEditorStore.getState().setEditor({ adjustments: pending });
  debouncedSetHistory(pending);
  // Committing the pending edit evicts the clicked baseline. Returning to
  // it becomes a new edit, with the pending edit kept immediately before it.
  useEditorStore.getState().goToHistoryIndex(0);
  expect(getUndoLabel()).toBe("Undo Edit");
  await undo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(50);
  await undo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(49);
  expect(getUndoLabel()).toBe("Undo Set Rating");
});

it("keeps slot order when lens or mask data rewrites history objects", async () => {
  await catalogAction("rating", async () => changes);
  const edited = { ...INITIAL_ADJUSTMENTS, exposure: 2 };
  useEditorStore.getState().setEditor({ adjustments: edited });
  useEditorStore.getState().pushHistory(edited);
  const state = useEditorStore.getState();
  useEditorStore.setState({
    history: state.history.map((recipe) => ({
      ...recipe,
      lensProfileFor: "resolved",
    })),
  });
  expect(getUndoLabel()).toBe("Undo Edit");
  await undo();
  expect(getUndoLabel()).toBe("Undo Set Rating");
});

it("orders Library metadata and file-journal steps together", async () => {
  const { fileAction, fileOperationTravel } = await import("./appUndo");
  useUIStore.setState({ activeView: "library" });
  await catalogAction("rating", async () => changes);
  const moved = { id: "move-1", kind: "move" };
  await fileAction(async () => moved);
  const undoFile = vi.fn(async () => {
    fileOperationTravel("undo", moved);
  });
  await undo(undoFile);
  expect(undoFile).toHaveBeenCalledOnce();
  expect(invoke).not.toHaveBeenCalled();
  await undo();
  expect(invoke).toHaveBeenCalledWith("restore_culling", expect.anything());
  vi.mocked(invoke).mockClear();
  await redo();
  expect(invoke).toHaveBeenCalledWith("restore_culling", expect.anything());
  const redoFile = vi.fn(async () => {
    fileOperationTravel("redo", moved);
  });
  await redo(redoFile);
  expect(redoFile).toHaveBeenCalledOnce();
});

it.each([false, true])(
  "a failed metadata restore retries before older file undo (tracked: %s)",
  async (tracked) => {
    const { fileAction, registerFileUndoHandlers } = await import("./appUndo");
    useUIStore.setState({ activeView: "library" });
    if (tracked) await fileAction(async () => ({ id: "older", kind: "move" }));
    const undoFile = vi.fn();
    const unregister = registerFileUndoHandlers({
      undo: undoFile,
      redo: vi.fn(),
    });
    try {
      await catalogAction("rating", async () => changes);
      vi.mocked(invoke).mockRejectedValue({ message: "catalog locked" });
      await undo();
      await undo();
      expect(undoFile).not.toHaveBeenCalled();
      expect(invoke).toHaveBeenCalledTimes(2);
      expect(toast.error).toHaveBeenCalledTimes(2);
      expect(useCatalogUndoStore.getState().past).toHaveLength(1);
    } finally {
      unregister();
    }
  },
);

it("keeps the history when the open session moves: rating → move → undo move → undo rating", async () => {
  const { fileAction, fileOperationTravel, followLibraryMove } = await import("./appUndo");
  useUIStore.setState({ activeView: "library" });
  const rated = {
    ...changes[0],
    path: "/job/Capture/a.nef",
    assetId: "asset-a",
    directory: "/job",
  };
  await catalogAction("rating", async () => [rated]);
  const moved = {
    id: "move-session-1",
    kind: "moveSession",
    moves: [{ from: "/job", to: "/archive/job" }],
  };
  // The session reopens at its new place (SessionWorkspace, `session-moved`).
  const reopened = (from: string, to: string) => {
    followLibraryMove(from, to);
    useLibraryStore.setState({ sessionDirectory: to });
  };
  await fileAction(async () => moved);
  reopened("/job", "/archive/job");
  expect(useCatalogUndoStore.getState().past).toHaveLength(1);
  expect(useCatalogUndoStore.getState().past[0].changes[0].path).toBe("/archive/job/Capture/a.nef");
  await undo(async () => {
    fileOperationTravel("undo", moved);
    reopened("/archive/job", "/job");
  });
  expect(useCatalogUndoStore.getState().filesFuture).toHaveLength(1);
  await undo();
  expect(invoke).toHaveBeenLastCalledWith("restore_culling", {
    directory: "/job",
    values: [{ path: "/job/Capture/a.nef", assetId: "asset-a", directory: "/job", value: rated.before }],
  });
  expect(useCatalogUndoStore.getState().future).toHaveLength(1);
  // Opening another library still starts a history of its own.
  useLibraryStore.setState({ sessionDirectory: "/another" });
  expect(useCatalogUndoStore.getState().future).toHaveLength(0);
  expect(useCatalogUndoStore.getState().filesFuture).toHaveLength(0);
});

it("a new metadata branch prevents the discarded native file redo from returning", async () => {
  const { fileAction, fileOperationTravel } = await import("./appUndo");
  useUIStore.setState({ activeView: "library" });
  const moved = { id: "move-1", kind: "move" };
  await fileAction(async () => moved);
  await undo(async () => {
    fileOperationTravel("undo", moved);
  });
  await catalogAction("rating", async () => changes);
  const redoFile = vi.fn();
  await redo(redoFile);
  expect(redoFile).not.toHaveBeenCalled();
});

it("keeps file redo discarded if a new metadata action arrived during file undo", async () => {
  const { fileAction, fileOperationTravel, undoBranchId } =
    await import("./appUndo");
  useUIStore.setState({ activeView: "library" });
  const moved = { id: "move-1", kind: "move" };
  await fileAction(async () => moved);
  let finish!: () => void;
  const oldBranch = undoBranchId();
  const undone = undo(
    () =>
      new Promise<void>((resolve) => {
        finish = () => {
          fileOperationTravel("undo", moved, oldBranch);
          resolve();
        };
      }),
  );
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  const rated = catalogAction("rating", async () => changes);
  finish();
  await undone;
  await rated;
  expect(useCatalogUndoStore.getState().filesFuture).toEqual([]);
  expect(useCatalogUndoStore.getState().fileRedoBlocked).toBe(true);
});

it("carries a receipt owner even when no library is selected in the UI", async () => {
  useLibraryStore.setState({ sessionDirectory: null });
  await catalogAction("rating", async () => [
    { ...changes[0], assetId: "asset-a", directory: "/implicit-owner" },
  ]);
  await undo();
  expect(invoke).toHaveBeenCalledWith("restore_culling", {
    directory: null,
    values: [
      {
        path: "/a.nef",
        assetId: "asset-a",
        directory: "/implicit-owner",
        value: changes[0].before,
      },
    ],
  });
});

it("a discarded file redo remains hidden after replaying an older file step", async () => {
  const { fileAction, fileOperationTravel } = await import("./appUndo");
  useUIStore.setState({ activeView: "library" });
  const older = { id: "older", kind: "rename" };
  const discarded = { id: "discarded", kind: "move" };
  await fileAction(async () => older);
  await fileAction(async () => discarded);
  await undo(async () => {
    fileOperationTravel("undo", discarded);
  });
  await catalogAction("rating", async () => changes);
  await undo();
  await undo(async () => {
    fileOperationTravel("undo", older);
  });
  await redo(async () => {
    fileOperationTravel("redo", older);
  });
  await redo();
  const staleRedo = vi.fn();
  await redo(staleRedo);
  expect(staleRedo).not.toHaveBeenCalled();
});

it("a catalog action queued during a move follows the committed new path", async () => {
  const { fileAction } = await import("./appUndo");
  useUIStore.setState({ activeView: "library" });
  let finish!: (value: {
    id: string;
    kind: string;
    moves: { from: string; to: string }[];
  }) => void;
  const moving = fileAction(
    () =>
      new Promise<{
        id: string;
        kind: string;
        moves: { from: string; to: string }[];
      }>((resolve) => {
        finish = resolve;
      }),
  );
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  let target: string[] = [];
  const rated = catalogAction("rating", async (context) => {
    target = context.resolvePaths(["/a.nef"]);
    return changes.map((change) => ({ ...change, path: target[0] }));
  });
  finish({
    id: "move",
    kind: "move",
    moves: [{ from: "/a.nef", to: "/Selects/a.nef" }],
  });
  await moving;
  await rated;
  expect(target).toEqual(["/Selects/a.nef"]);
});

it("settles an older pending edit before Library metadata undo so redo stays available", async () => {
  useUIStore.setState({ activeView: "library" });
  await catalogAction("rating", async () => changes);
  const edited = { ...INITIAL_ADJUSTMENTS, exposure: 2 };
  useEditorStore.getState().setEditor({ adjustments: edited });
  debouncedSetHistory(edited);
  let finish!: (value: unknown) => void;
  vi.mocked(invoke).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const undone = undo();
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  debouncedSetHistory.flush();
  finish([{ ...changes[0], after: changes[0].before }]);
  await undone;
  expect(useCatalogUndoStore.getState().future).toHaveLength(1);
});

it("saves and undoes unrelated metadata while a move is still running", async () => {
  const { fileAction } = await import("./appUndo");
  let finish!: (value: { id: string; kind: string }) => void;
  const moving = fileAction(
    () =>
      new Promise<{ id: string; kind: string }>((resolve) => {
        finish = resolve;
      }),
    "move",
    undefined,
    undefined,
    ["/other"],
  );
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  const blocked = catalogAction(
    "rating",
    async () => changes.map((change) => ({ ...change, path: "/other/a.nef" })),
    ["/other/a.nef"],
  );
  let saved = false;
  const rated = catalogAction(
    "rating",
    async () => {
      saved = true;
      return changes;
    },
    ["/a.nef"],
  );
  try {
    await vi.waitFor(() => expect(saved).toBe(true), { timeout: 100 });
    await rated;
    const undone = undo();
    await vi.waitFor(
      () =>
        expect(invoke).toHaveBeenCalledWith(
          "restore_culling",
          expect.anything(),
        ),
      { timeout: 100 },
    );
    await undone;
  } finally {
    finish({ id: "move", kind: "move" });
    await moving;
    await blocked;
    await rated;
  }
});
it("names an off-screen photo in undo and redo notices without navigating", async () => {
  await catalogAction("rating", async () => changes);
  useEditorStore.setState({
    selectedImage: { path: "/b.nef", isReady: true } as never,
  });
  await undo();
  expect(toast.info).toHaveBeenLastCalledWith("Undo Set Rating: a.nef", {
    autoClose: 3000,
  });
  await redo();
  expect(toast.info).toHaveBeenLastCalledWith("Redo Set Rating: a.nef", {
    autoClose: 3000,
  });
  expect(useEditorStore.getState().selectedImage?.path).toBe("/b.nef");
});

it.each(["/folder/a.nef", "/folder/a.nef?vc=copy"])(
  "waits for an overlapping folder move before writing %s",
  async (path) => {
    const { fileAction } = await import("./appUndo");
    let finish!: (value: {
      id: string;
      moves: { from: string; to: string }[];
    }) => void;
    const moving = fileAction(
      () =>
        new Promise<{ id: string; moves: { from: string; to: string }[] }>(
          (resolve) => {
            finish = resolve;
          },
        ),
      "move",
      undefined,
      undefined,
      ["/folder", "/moved"],
    );
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    const save = vi.fn(
      async ({ resolvePaths }: import("./appUndo").CatalogActionContext) =>
        changes.map((change) => ({ ...change, path: resolvePaths([path])[0] })),
    );
    const rated = catalogAction("rating", save, [path]);
    await Promise.resolve();
    expect(save).not.toHaveBeenCalled();
    finish({ id: "move", moves: [{ from: "/folder", to: "/moved" }] });
    await moving;
    await rated;
    expect(useCatalogUndoStore.getState().past[0].changes[0].path).toBe(
      path.replace("/folder", "/moved"),
    );
  },
);

it("waits when an undo target moves again after its receipt path became stale", async () => {
  const { fileAction } = await import("./appUndo");
  await catalogAction("rating", async () => changes, ["/a.nef"]);
  await fileAction(
    async () => ({ id: "first", moves: [{ from: "/a.nef", to: "/b.nef" }] }),
    "move",
    undefined,
    undefined,
    ["/a.nef", "/b.nef"],
  );
  let finish!: (value: {
    id: string;
    moves: { from: string; to: string }[];
  }) => void;
  const moving = fileAction(
    () =>
      new Promise<{ id: string; moves: { from: string; to: string }[] }>(
        (resolve) => {
          finish = resolve;
        },
      ),
    "move",
    undefined,
    undefined,
    ["/b.nef", "/c.nef"],
  );
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  const undone = undo();
  try {
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(invoke).not.toHaveBeenCalled();
  } finally {
    finish({ id: "second", moves: [{ from: "/b.nef", to: "/c.nef" }] });
    await moving;
    await undone;
  }
});

it("waits for both older moves in a chain before writing its original path", async () => {
  const { fileAction } = await import("./appUndo");
  type Move = { id: string; moves: { from: string; to: string }[] };
  let finishFirst!: (value: Move) => void;
  let finishSecond!: (value: Move) => void;
  const first = fileAction(
    () =>
      new Promise<Move>((resolve) => {
        finishFirst = resolve;
      }),
    "move",
    undefined,
    undefined,
    ["/a.nef", "/b.nef"],
  );
  await vi.waitFor(() => expect(finishFirst).toBeTypeOf("function"));
  const second = fileAction(
    () =>
      new Promise<Move>((resolve) => {
        finishSecond = resolve;
      }),
    "move",
    undefined,
    undefined,
    ["/b.nef", "/c.nef"],
  );
  const save = vi.fn(
    async ({ resolvePaths }: import("./appUndo").CatalogActionContext) =>
      changes.map((change) => ({
        ...change,
        path: resolvePaths(["/a.nef"])[0],
      })),
  );
  const rated = catalogAction("rating", save, ["/a.nef"]);
  finishFirst({ id: "first", moves: [{ from: "/a.nef", to: "/b.nef" }] });
  await first;
  await vi.waitFor(() => expect(finishSecond).toBeTypeOf("function"));
  try {
    expect(save).not.toHaveBeenCalled();
  } finally {
    finishSecond({ id: "second", moves: [{ from: "/b.nef", to: "/c.nef" }] });
    await second;
    await rated;
  }
  expect(useCatalogUndoStore.getState().past[0].changes[0].path).toBe("/c.nef");
});

it("keeps on-screen metadata undo quiet", async () => {
  await catalogAction("rating", async () => changes);
  await undo();
  expect(toast.info).not.toHaveBeenCalled();
});
it("names an off-screen batch through the translated notice", async () => {
  await catalogAction("rating", async () => [
    changes[0],
    { ...changes[0], path: "/other.nef" },
  ]);
  useEditorStore.setState({
    selectedImage: { path: "/b.nef", isReady: true } as never,
  });
  await undo();
  expect(toast.info).toHaveBeenLastCalledWith("Undo Set Rating: a.nef (+1)", {
    autoClose: 3000,
  });
});

it("skips a failed restore only on an explicit click, without executing older undo", async () => {
  const { fileAction, registerFileUndoHandlers } = await import("./appUndo");
  useUIStore.setState({ activeView: "library" });
  await fileAction(async () => ({ id: "older", kind: "move" }));
  await catalogAction("rating", async () => changes);
  const undoFile = vi.fn();
  const unregister = registerFileUndoHandlers({
    undo: undoFile,
    redo: vi.fn(),
  });
  vi.mocked(invoke).mockRejectedValueOnce({
    message: "missing source",
    failed: ["/a.nef"],
    culling: [],
  });
  await undo();
  expect(undoFile).not.toHaveBeenCalled();
  const notice = vi.mocked(toast.error).mock.calls.at(-1)![0];
  expect(notice).toBeTypeOf("function");
  const closeToast = vi.fn();
  const view = render(
    (
      notice as (props: { closeToast: () => void }) => import("react").ReactNode
    )({ closeToast }),
  );
  try {
    expect(view.getByText(/a.nef/)).toBeTruthy();
    fireEvent.click(view.getByRole("button", { name: "Skip failed items" }));
    expect(useCatalogUndoStore.getState().past).toEqual([]);
    expect(undoFile).not.toHaveBeenCalled();
    await undo();
    expect(undoFile).toHaveBeenCalledOnce();
  } finally {
    view.unmount();
    unregister();
  }
});
it("keeps successful partial restores redoable while failed targets wait for retry or Skip", async () => {
  const missing = { ...changes[0], path: "/missing.nef" };
  await catalogAction("rating", async () => [changes[0], missing]);
  vi.mocked(invoke).mockRejectedValueOnce({
    message: "missing source",
    failed: [missing.path],
    culling: [{ ...changes[0], after: changes[0].before }],
  });
  await undo();
  expect(useCatalogUndoStore.getState().past[0].changes).toEqual([missing]);
  expect(useCatalogUndoStore.getState().future[0].changes).toEqual(changes);
  expect(canRedo()).toBe(true);
  const notice = vi.mocked(toast.error).mock.calls.at(-1)![0];
  await redo();
  expect(invoke).toHaveBeenLastCalledWith("restore_culling", {
    directory: "/job",
    values: [{ path: "/a.nef", value: changes[0].after }],
  });
  const view = render(
    (
      notice as (props: { closeToast: () => void }) => import("react").ReactNode
    )({ closeToast: vi.fn() }),
  );
  try {
    fireEvent.click(view.getByRole("button", { name: "Skip failed items" }));
    expect(useCatalogUndoStore.getState().past).toHaveLength(1);
    expect(useCatalogUndoStore.getState().past[0].changes).toEqual(changes);
  } finally {
    view.unmount();
  }
});

it("retries only the failed part of a partial restore", async () => {
  const missing = { ...changes[0], path: "/missing.nef" };
  await catalogAction("rating", async () => [changes[0], missing]);
  vi.mocked(invoke).mockRejectedValueOnce({
    message: "offline",
    culling: [{ ...changes[0], after: changes[0].before }],
  });
  await undo();
  await undo();
  expect(invoke).toHaveBeenLastCalledWith("restore_culling", {
    directory: "/job",
    values: [{ path: missing.path, value: missing.before }],
  });
  expect(useCatalogUndoStore.getState().past).toEqual([]);
  expect(
    useCatalogUndoStore.getState().future.flatMap((step) => step.changes),
  ).toEqual([changes[0], missing]);
});
it("matches partial restore receipts by managed identity after rename, keeping virtual copies separate", async () => {
  const first = {
    ...changes[0],
    path: "/old.nef?vc=one",
    assetId: "asset",
    directory: "/job",
  };
  const second = { ...first, path: "/old.nef?vc=two" };
  await catalogAction("rating", async () => [first, second]);
  vi.mocked(invoke).mockRejectedValueOnce({
    message: "copy unavailable",
    culling: [{ ...first, path: "/renamed.nef?vc=one", after: first.before }],
  });
  await undo();
  expect(useCatalogUndoStore.getState().past[0].changes).toEqual([second]);
  expect(useCatalogUndoStore.getState().future[0].changes).toEqual([first]);
});
it("a stale Skip notice cannot discard history after switching libraries", async () => {
  await catalogAction("rating", async () => changes);
  vi.mocked(invoke).mockRejectedValueOnce({ message: "offline" });
  await undo();
  const notice = vi.mocked(toast.error).mock.calls.at(-1)![0] as (props: {
    closeToast: () => void;
  }) => import("react").ReactNode;
  useLibraryStore.setState({ sessionDirectory: "/another" });
  await catalogAction("rating", async () => changes);
  const view = render(notice({ closeToast: vi.fn() }));
  try {
    fireEvent.click(view.getByRole("button", { name: "Skip failed items" }));
    expect(useCatalogUndoStore.getState().past).toHaveLength(1);
  } finally {
    view.unmount();
  }
});

it("keeps a visible virtual copy's physical flag undo quiet", async () => {
  await catalogAction("flag", async () => [
    {
      path: "/a.nef",
      before: { kind: "flag", value: 0 },
      after: { kind: "flag", value: 1 },
    },
  ]);
  useEditorStore.setState({
    selectedImage: { path: "/a.nef?vc=one", isReady: true } as never,
  });
  await undo();
  expect(toast.info).not.toHaveBeenCalled();
});
it("also splits a partially failed redo and retries only its failed targets", async () => {
  const missing = { ...changes[0], path: "/missing.nef" };
  await catalogAction("rating", async () => [changes[0], missing]);
  await undo();
  vi.mocked(invoke).mockRejectedValueOnce({
    message: "offline",
    culling: changes,
  });
  await redo();
  expect(useCatalogUndoStore.getState().past[0].changes).toEqual(changes);
  expect(useCatalogUndoStore.getState().future[0].changes).toEqual([missing]);
  await redo();
  expect(invoke).toHaveBeenLastCalledWith("restore_culling", {
    directory: "/job",
    values: [{ path: missing.path, value: missing.after }],
  });
});

it("a stale Skip cannot discard a healthy step after retry success and redo", async () => {
  await catalogAction("rating", async () => changes);
  vi.mocked(invoke).mockRejectedValueOnce({ message: "offline" });
  await undo();
  const notice = vi.mocked(toast.error).mock.calls.at(-1)![0] as (props: {
    closeToast: () => void;
  }) => import("react").ReactNode;
  await undo();
  await redo();
  expect(useCatalogUndoStore.getState().past).toHaveLength(1);
  const view = render(notice({ closeToast: vi.fn() }));
  try {
    fireEvent.click(view.getByRole("button", { name: "Skip failed items" }));
    expect(useCatalogUndoStore.getState().past).toHaveLength(1);
  } finally {
    view.unmount();
  }
});
it("updates one persistent notice on repeated failures and dismisses it on success", async () => {
  await catalogAction("rating", async () => changes);
  vi.mocked(invoke).mockRejectedValueOnce({ message: "offline" });
  await undo();
  const options = vi.mocked(toast.error).mock.calls.at(-1)![1];
  expect(options?.toastId).toBeTruthy();
  vi.mocked(toast.isActive).mockReturnValueOnce(true);
  vi.mocked(invoke).mockRejectedValueOnce({ message: "still offline" });
  await undo();
  expect(toast.error).toHaveBeenCalledTimes(1);
  expect(toast.update).toHaveBeenCalledWith(
    options!.toastId,
    expect.objectContaining({ render: expect.any(Function) }),
  );
  await undo();
  expect(toast.dismiss).toHaveBeenCalledWith(options!.toastId);
});

it("an obsolete failure callback cannot invalidate the current Skip action", async () => {
  await catalogAction("rating", async () => changes);
  vi.mocked(invoke).mockRejectedValueOnce({ message: "offline" });
  await undo();
  const oldNotice = vi.mocked(toast.error).mock.calls.at(-1)![0] as (props: {
    closeToast: () => void;
  }) => import("react").ReactNode;
  vi.mocked(invoke).mockRejectedValueOnce({ message: "still offline" });
  await undo();
  const currentNotice = vi
    .mocked(toast.error)
    .mock.calls.at(-1)![0] as typeof oldNotice;
  const oldView = render(oldNotice({ closeToast: vi.fn() }));
  fireEvent.click(oldView.getByRole("button", { name: "Skip failed items" }));
  oldView.unmount();
  expect(useCatalogUndoStore.getState().past).toHaveLength(1);
  const currentView = render(currentNotice({ closeToast: vi.fn() }));
  try {
    fireEvent.click(
      currentView.getByRole("button", { name: "Skip failed items" }),
    );
    expect(useCatalogUndoStore.getState().past).toEqual([]);
  } finally {
    currentView.unmount();
  }
});

it("a batch recipe change (series match) is one undo step for all its photos", async () => {
  // A backend of recipes: the writer returns what it replaced.
  const recipes: Record<string, Record<string, unknown>> = {
    "/a.nef": { exposure: 0.5 },
    "/b.nef": {},
    "/c.nef": { exposure: -1, whiteBalance: "auto", wbTemperature: 5000, wbTint: 4 },
  };
  const writes: RecipeValues[][] = [];
  registerRecipeWriter(async (values) => {
    writes.push(values);
    return values.map(({ path, values: next }) => {
      const before = Object.fromEntries(Object.keys(next).map((key) => [key, recipes[path][key] ?? null]));
      for (const [key, value] of Object.entries(next)) {
        if (value === null) delete recipes[path][key];
        else recipes[path][key] = value;
      }
      return { path, values: before };
    });
  });
  useUIStore.setState({ activeView: "library" });
  const original = structuredClone(recipes);
  const written = await recipeAction([
    { path: "/a.nef", values: { exposure: 1.2 } },
    { path: "/b.nef", values: { exposure: 0.4, whiteBalance: "custom", wbTemperature: 3400, wbTint: 6 } },
    { path: "/c.nef", values: { whiteBalance: "custom", wbTemperature: 4100, wbTint: 2 } },
  ]);
  expect(written).toEqual(["/a.nef", "/b.nef", "/c.nef"]);
  const matched = structuredClone(recipes);
  expect(getUndoLabel()).toBe("Undo Match Light");
  await undo();
  expect(writes).toHaveLength(2);
  expect(recipes).toEqual(original);
  expect(canRedo()).toBe(true);
  await redo();
  expect(recipes).toEqual(matched);
});

it("photos not written by a batch recipe change are not part of its undo", async () => {
  registerRecipeWriter(async (values) =>
    values.filter(({ path }) => path !== "/b.nef").map(({ path }) => ({ path, values: { exposure: 0 } })),
  );
  useUIStore.setState({ activeView: "library" });
  const written = await recipeAction([
    { path: "/a.nef", values: { exposure: 1 } },
    { path: "/b.nef", values: { exposure: 2 } },
  ]);
  expect(written).toEqual(["/a.nef"]);
  const writer = vi.fn(async (values: RecipeValues[]) => values);
  registerRecipeWriter(writer);
  await undo();
  expect(writer).toHaveBeenCalledWith([{ path: "/a.nef", values: { exposure: 0 } }], expect.anything());
});

/** Recipes on "disk" for the batch tests, written by a registered replacer. */
function batchDisk(recipes: Record<string, unknown>) {
  const replacer = vi.fn(
    async (written: { path: string; adjustments: unknown }[], _shown: RecipeShown) => {
      for (const { path, adjustments } of written) recipes[path] = adjustments;
      return written.map(({ path }) => path);
    },
  );
  registerRecipeWriter(async (values) => values, replacer);
  const snapshot = (paths: string[], after: (path: string) => unknown) =>
    paths.map((path) => {
      const before = recipes[path];
      recipes[path] = after(path);
      return { path, before, after: recipes[path] };
    });
  return { replacer, snapshot };
}

it("one undo restores every photo a Sync changed, and redo applies it again (O-01)", async () => {
  const recipes: Record<string, unknown> = {
    "/b.nef": { exposure: 0.2 },
    "/c.nef": {},
  };
  const { replacer, snapshot } = batchDisk(recipes);
  const edited = { ...INITIAL_ADJUSTMENTS, exposure: 1.85 };
  useEditorStore.getState().setEditor({ adjustments: edited });
  debouncedSetHistory(edited);
  debouncedSetHistory.flush();
  await recipeBatchAction(
    "sync",
    async () => ({
      failed: [],
      changes: snapshot(["/b.nef", "/c.nef"], () => ({ exposure: 1.85 })),
    }),
    ["/b.nef", "/c.nef"],
  );
  expect(getUndoLabel()).toBe("Undo Sync Settings");
  await undo();
  expect(recipes).toEqual({ "/b.nef": { exposure: 0.2 }, "/c.nef": {} });
  // The open photograph keeps its own edit; it is the next step.
  expect(useEditorStore.getState().adjustments.exposure).toBe(1.85);
  expect(getUndoLabel()).toBe("Undo Edit");
  await redo();
  expect(recipes).toEqual({
    "/b.nef": { exposure: 1.85 },
    "/c.nef": { exposure: 1.85 },
  });
  expect(replacer).toHaveBeenCalledTimes(2);
});

it("a paste onto the open photo and others is one undo step for all of them (O-01)", async () => {
  const recipes: Record<string, unknown> = { "/b.nef": { contrast: 3 } };
  const { replacer, snapshot } = batchDisk(recipes);
  const pasted = { ...INITIAL_ADJUSTMENTS, exposure: 0.7 };
  useEditorStore.getState().setEditor({ adjustments: pasted });
  useEditorStore.getState().pushHistory(pasted);
  const { history, historyIndex } = useEditorStore.getState();
  await recipeBatchAction(
    "paste",
    async () => ({
      failed: [],
      changes: snapshot(["/b.nef"], () => ({ contrast: 3, exposure: 0.7 })),
    }),
    ["/b.nef"],
    editUndoOrder(history, historyIndex),
  );
  expect(getUndoLabel()).toBe("Undo Paste Settings");
  await undo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
  expect(recipes["/b.nef"]).toEqual({ contrast: 3 });
  // The open photograph travelled in its editor, not by loading it again.
  expect(replacer.mock.calls[0][1]).toMatchObject({ skipOpen: true });
  expect(canUndo()).toBe(false);
  await redo();
  expect(useEditorStore.getState().adjustments.exposure).toBe(0.7);
  expect(recipes["/b.nef"]).toEqual({ contrast: 3, exposure: 0.7 });
  expect(canRedo()).toBe(false);
});
