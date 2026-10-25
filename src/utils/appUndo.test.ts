import "../i18n";
import { beforeEach, expect, it, vi } from "vitest";
import { toast } from "react-toastify";
import { invoke } from "@tauri-apps/api/core";
import { debouncedSetHistory, useEditorStore } from "../store/useEditorStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { useUIStore } from "../store/useUIStore";
import { INITIAL_ADJUSTMENTS } from "./adjustments";
import {
  catalogAction,
  undo,
  redo,
  canRedo,
  getUndoLabel,
  useCatalogUndoStore,
} from "./appUndo";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn(), info: vi.fn() } }));
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
