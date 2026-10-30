import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useTauriListeners } from "./useTauriListeners";
import { useProcessStore } from "../store/useProcessStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { CLOSED_MERGE_MODAL, useUIStore } from '../store/useUIStore';
import type { ImageFile, SelectedImage } from '../components/ui/AppProperties';
import { invoke } from "@tauri-apps/api/core";
const callbacks = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name, callback) => {
    callbacks.set(name, callback);
    return Promise.resolve(() => {});
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => `asset://${path}`,
  invoke: vi.fn((command: string) =>
    Promise.resolve(
      command === "export_queue_jobs"
        ? { jobs: exportQueue, held: false }
        : command === "edit_with_interrupted"
          ? []
          : exportQueue,
    ),
  ),
}));
let exportQueue: unknown[] = [];
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  useEditorStore.getState().setEditor({ selectedImage: null, hasRenderedFirstFrame: false });
});

it('fills only the currently loading source preview, preserving embedded and completed previews', () => {
  vi.useFakeTimers();
  vi.stubGlobal('requestAnimationFrame', (fn: () => void) => setTimeout(fn, 16));
  vi.stubGlobal('cancelAnimationFrame', clearTimeout);
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  const publish = (path: string, previewPath: string) => act(() => {
    callbacks.get('thumbnail-generated')!({ payload: { path, thumbnailPath: '/small.jpg', previewPath } });
  });
  useEditorStore.getState().setEditor({ selectedImage: { path: '/a.dng', isReady: false } as SelectedImage });
  publish('/a.dng', '/a-medium.jpg');
  // Selection changes before the animation-frame batch is flushed.
  useEditorStore.getState().setEditor({ selectedImage: { path: '/b.dng', isReady: false } as SelectedImage });
  act(() => vi.advanceTimersByTime(20));
  expect(useEditorStore.getState().selectedImage?.thumbnailUrl).toBeUndefined();
  publish('/b.dng', '/b-medium.jpg');
  act(() => vi.advanceTimersByTime(20));
  expect(useEditorStore.getState().selectedImage?.thumbnailUrl).toBe('asset:///b-medium.jpg');
  for (const protectedState of [
    { isReady: true }, { isReady: false, thumbnailSource: 'embedded' },
  ]) {
    useEditorStore.getState().setEditor({ selectedImage: {
      path: '/b.dng', thumbnailUrl: 'protected', ...protectedState,
    } as SelectedImage });
    publish('/b.dng', '/late.jpg');
    act(() => vi.advanceTimersByTime(20));
    expect(useEditorStore.getState().selectedImage?.thumbnailUrl).toBe('protected');
  }
  useEditorStore.getState().setEditor({
    hasRenderedFirstFrame: true,
    selectedImage: { path: '/b.dng', isReady: false, thumbnailUrl: 'rendered' } as SelectedImage,
  });
  publish('/b.dng', '/late-after-frame.jpg');
  act(() => vi.advanceTimersByTime(20));
  expect(useEditorStore.getState().selectedImage?.thumbnailUrl).toBe('rendered');
});
it("publishes completed thumbnails without replacing unchanged large-library metadata", () => {
  vi.useFakeTimers();
  vi.stubGlobal("requestAnimationFrame", (fn: () => void) =>
    setTimeout(fn, 16),
  );
  vi.stubGlobal("cancelAnimationFrame", clearTimeout);
  const images = Array.from({ length: 20596 }, (_, i) => ({
    path: `/image-${i}.RAF`,
    rating: 0,
    is_edited: false,
  })) as ImageFile[];
  useLibraryStore
    .getState()
    .setLibrary({ imageList: images, imageRatings: {} });
  useProcessStore
    .getState()
    .setProcess({ thumbnails: {}, mediumThumbnails: {} });
  const ratings = useLibraryStore.getState().imageRatings;
  const markGenerated = vi.fn();
  renderHook(() =>
    useTauriListeners({
      refreshAllFolderTrees: vi.fn(),
      handleSelectSubfolder: vi.fn(),
      refreshImageList: vi.fn(),
      markGenerated,
    }),
  );
  act(() => {
    callbacks.get("thumbnail-generated")!({
      payload: {
        path: "/image-5.RAF",
        thumbnailPath: "/small.jpg",
        previewPath: "/medium.jpg",
        rating: 0,
        is_edited: false,
      },
    });
    vi.advanceTimersByTime(20);
  });
  expect(useProcessStore.getState().thumbnails["/image-5.RAF"]).toBe(
    "asset:///small.jpg",
  );
  expect(useLibraryStore.getState().imageList).toBe(images);
  expect(useLibraryStore.getState().imageRatings).toBe(ratings);
  expect(markGenerated).toHaveBeenCalledWith("/image-5.RAF");
});

it("keeps newer listed ratings over a thumbnail's rating read when its generation began", () => {
  vi.useFakeTimers();
  vi.stubGlobal("requestAnimationFrame", (fn: () => void) => setTimeout(fn, 16));
  vi.stubGlobal("cancelAnimationFrame", clearTimeout);
  // C2 was listed unrated (not yet registered), its thumbnail began then;
  // Synchronize Folder followed the photograph to it and the list came back
  // with its rating before the thumbnail finished.
  const images = [
    { path: "/Shoot/C2.ORF", rating: 3, is_edited: true },
    { path: "/Shoot/D.ORF", rating: 2, is_edited: false },
  ] as ImageFile[];
  useLibraryStore.getState().setLibrary({ imageList: images, imageRatings: { "/Shoot/C2.ORF": 3 } });
  renderHook(() =>
    useTauriListeners({
      refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
      refreshImageList: vi.fn(), markGenerated: vi.fn(),
    }),
  );
  const thumbnail = (path: string, rating: number | null, is_edited: boolean | null = false) =>
    callbacks.get("thumbnail-generated")!({
      payload: { path, thumbnailPath: "/small.jpg", previewPath: "/medium.jpg", rating, is_edited },
    });
  act(() => {
    // Generated before C2 had a sidecar: nothing known about it.
    thumbnail("/Shoot/C2.ORF", null, null);
    thumbnail("/Shoot/D.ORF", 0);
    thumbnail("/Elsewhere/E.ORF", 4);
    vi.advanceTimersByTime(20);
  });
  const ratings = useLibraryStore.getState().imageRatings;
  expect(ratings["/Shoot/C2.ORF"]).toBe(3);
  expect(ratings["/Shoot/D.ORF"]).toBeUndefined();
  // A rating the listing did not bring is filled in.
  expect(ratings["/Elsewhere/E.ORF"]).toBe(4);
  expect(useLibraryStore.getState().imageList[0].is_edited).toBe(true);
  // A thumbnail generated after an edit still updates the edit state.
  act(() => {
    thumbnail("/Shoot/D.ORF", 2, true);
    vi.advanceTimersByTime(20);
  });
  expect(useLibraryStore.getState().imageList[1].is_edited).toBe(true);
});

it("does not re-query the library when selecting a photo with unchanged metadata, but publishes real changes", () => {
  const images = Array.from({ length: 20596 }, (_, i) => ({
    path: `/image-${i}.RAF`,
    rating: 0,
    is_edited: false,
    tags: ["user:studio"],
  })) as ImageFile[];
  useLibraryStore
    .getState()
    .setLibrary({ imageList: images, imageRatings: {} });
  const initial = useLibraryStore.getState();
  renderHook(() =>
    useTauriListeners({
      refreshAllFolderTrees: vi.fn(),
      handleSelectSubfolder: vi.fn(),
      refreshImageList: vi.fn(),
      markGenerated: vi.fn(),
    }),
  );
  const publish = (metadata: object) =>
    act(() =>
      callbacks.get("image-metadata-loaded")!({
        payload: {
          path: "/image-20000.RAF",
          rating: 0,
          is_edited: false,
          tags: ["user:studio"],
          ...metadata,
        },
      }),
    );
  publish({});
  expect(useLibraryStore.getState()).toBe(initial);
  publish({ rating: 4 });
  expect(useLibraryStore.getState().imageRatings["/image-20000.RAF"]).toBe(4);
  expect(useLibraryStore.getState().imageList).toBe(images);
  publish({ rating: 4, is_edited: true, tags: ["user:portrait"] });
  const changed = useLibraryStore.getState();
  expect(changed.imageList[20000]).toMatchObject({
    is_edited: true,
    tags: ["user:portrait"],
  });
  expect(changed.imageList[0]).toBe(images[0]);
  publish({ rating: 4, is_edited: true, tags: null });
  expect(useLibraryStore.getState()).toBe(changed);
  publish({ rating: 4, is_edited: true, tags: [] });
  expect(useLibraryStore.getState().imageList[20000].tags).toEqual([]);
});

it("takes a camera's rating from the facts pass only for photographs unrated and unwritten here", async () => {
  const { invokeInOrder } = await import("../utils/catalogWrites");
  const images = ["/a.RW2", "/b.RW2", "/c.RW2"].map((path) => ({ path, rating: 0, is_edited: false, tags: [] })) as unknown as ImageFile[];
  useLibraryStore.getState().setLibrary({ imageList: images, imageRatings: { "/b.RW2": 5 }, currentFolderPath: null });
  renderHook(() =>
    useTauriListeners({
      refreshAllFolderTrees: vi.fn(),
      handleSelectSubfolder: vi.fn(),
      refreshImageList: vi.fn(),
      markGenerated: vi.fn(),
    }),
  );
  // C was rated and set back to none in this window (its write may follow
  // the pass's read of it).
  await invokeInOrder("set_rating_for_paths", { paths: ["/c.RW2"], rating: 0 }).catch(() => {});
  const before = useLibraryStore.getState();
  await act(() =>
    callbacks.get("library-camera-ratings")!({
      payload: [["/a.RW2", 3], ["/b.RW2", 3], ["/c.RW2", 3], ["/elsewhere/d.RW2", 3]],
    }),
  );
  const { imageRatings } = useLibraryStore.getState();
  expect([imageRatings["/a.RW2"], imageRatings["/b.RW2"], imageRatings["/c.RW2"]]).toEqual([3, 5, undefined]);
  // Photographs not listed here are not kept.
  expect(Object.keys(imageRatings).sort()).toEqual(["/a.RW2", "/b.RW2"]);
  // One update for the batch; nothing to take, nothing changes.
  const after = useLibraryStore.getState();
  expect(after.imageRatings).not.toBe(before.imageRatings);
  await act(() => callbacks.get("library-camera-ratings")!({ payload: [["/a.RW2", 4]] }));
  expect(useLibraryStore.getState()).toBe(after);
});

it("finds a camera's rating for a photograph listed through a symlinked folder", async () => {
  const { invoke } = await import("@tauri-apps/api/core");
  const images = ["/alias/a.RW2", "/alias/day/b.RW2"].map((path) => ({ path, rating: 0, is_edited: false, tags: [] })) as unknown as ImageFile[];
  useLibraryStore.getState().setLibrary({ imageList: images, imageRatings: {}, currentFolderPath: "/alias" });
  const original = vi.mocked(invoke).getMockImplementation()!;
  const asked: unknown[] = [];
  vi.mocked(invoke).mockImplementation((command: string, args?: unknown) => {
    if (command !== "session_paths_as_listed") return original(command, args as never);
    asked.push(args);
    return Promise.resolve(["/alias/a.RW2", "/alias/day/b.RW2"]);
  });
  renderHook(() =>
    useTauriListeners({
      refreshAllFolderTrees: vi.fn(),
      handleSelectSubfolder: vi.fn(),
      refreshImageList: vi.fn(),
      markGenerated: vi.fn(),
    }),
  );
  await act(() =>
    callbacks.get("library-camera-ratings")!({ payload: [["/real/a.RW2", 2], ["/real/day/b.RW2", 4]] }),
  );
  vi.mocked(invoke).mockImplementation(original);
  expect(asked).toEqual([{ paths: ["/real/a.RW2", "/real/day/b.RW2"], listed: "/alias" }]);
  expect(useLibraryStore.getState().imageRatings).toEqual({ "/alias/a.RW2": 2, "/alias/day/b.RW2": 4 });
});

it("loads the export queue at start; an interrupted export waits under Needs attention with Resume", async () => {
  vi.useRealTimers();
  await import("../i18n");
  const { startActivityBridges } = await import("../activity/bridges");
  const { useActivityStore } = await import("../store/useActivityStore");
  const stop = startActivityBridges();
  exportQueue = [{ id: "j", label: "3 photos as JPG", state: "interrupted", createdAt: "", total: 3, done: 1, failed: 0 }];
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  await vi.waitFor(() => expect(useProcessStore.getState().exportQueue).toHaveLength(1));
  const entry = useActivityStore.getState().entries.find((e) => e.id === "export:j");
  expect(entry).toMatchObject({ status: "warning", source: "export" });
  expect(entry?.title).toContain("3 photos as JPG");
  expect(entry?.actions?.map((a) => a.label)).toEqual(["Resume"]);
  stop();
  exportQueue = [];
});

it("names AI Denoise on the export progress while a photo waits for it", async () => {
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  await vi.waitFor(() => expect(callbacks.has("export-denoising")).toBe(true));
  const send = (name: string, payload: unknown) => act(() => callbacks.get(name)!({ payload }));
  send("export-job-started", { current: 0, total: 2 });
  send("export-denoising", "/a.RAF");
  send("export-denoising", "/b.RAF?vc=1");
  expect(useProcessStore.getState().exportState.denoising).toEqual(["/a.RAF", "/b.RAF?vc=1"]);
  send("batch-export-progress", { current: 1, total: 2, path: "/a.RAF" });
  expect(useProcessStore.getState().exportState.denoising).toEqual(["/b.RAF?vc=1"]);
  send("batch-export-progress", { current: 2, total: 2, path: "/b.RAF?vc=1" });
  expect(useProcessStore.getState().exportState.denoising).toEqual([]);
  // A new export starts with none, whatever the last one left.
  send("export-denoising", "/c.RAF");
  send("export-job-started", { current: 0, total: 1 });
  expect(useProcessStore.getState().exportState.denoising).toEqual([]);
});

it("follows the running job's photos and the queue's hold for the queue panel", async () => {
  const { invoke } = await import("@tauri-apps/api/core");
  const photo = (index: number, state: string) => ({
    index, path: `/p/${index}.RAF`, state, output: null, error: null, attempts: 0, startedMs: null, finishedMs: null,
  });
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  await vi.waitFor(() => expect(callbacks.has("export-item-progress")).toBe(true));
  const send = (name: string, payload: unknown) => act(() => callbacks.get(name)!({ payload }));
  const job = { id: "j", label: "2 photos as JPG", createdAt: "", total: 2, done: 0, failed: 0, failures: [] };
  // Photos of a job nobody read are not followed.
  send("export-item-progress", { job: "j", item: photo(0, "running"), stage: "loading", fraction: null });
  expect(useProcessStore.getState().exportItems.j).toBeUndefined();
  exportQueue = [photo(0, "pending"), photo(1, "pending")];
  send("export-job-started", { id: "j", ids: ["j", "j2"], current: 0, total: 4 });
  await vi.waitFor(() => expect(useProcessStore.getState().exportItems.j).toHaveLength(2));
  expect(invoke).toHaveBeenCalledWith("export_queue_items", { job: "j" });
  // A recipe running with it in the same develop is read too.
  expect(invoke).toHaveBeenCalledWith("export_queue_items", { job: "j2" });
  send("export-item-progress", { job: "j", item: photo(0, "running"), stage: "denoising", fraction: 0.25 });
  expect(useProcessStore.getState().exportItems.j[0]).toMatchObject({ state: "running", stage: "denoising", fraction: 0.25 });
  send("export-queue-changed", { jobs: [{ ...job, state: "running" }], held: true });
  send("export-job-pausing", "j");
  expect(useProcessStore.getState().exportPausing).toEqual(["j"]);
  expect(useProcessStore.getState().exportQueueHeld).toBe(true);
  // The run ended: the photos are read again and the pause is over.
  exportQueue = [photo(0, "done"), photo(1, "pending")];
  send("export-queue-changed", { jobs: [{ ...job, state: "paused", done: 1 }], held: false });
  expect(useProcessStore.getState().exportPausing).toEqual([]);
  await vi.waitFor(() => expect(useProcessStore.getState().exportItems.j[0].state).toBe("done"));
  // A pause that comes after the job ended is not shown.
  send("export-job-pausing", "j");
  expect(useProcessStore.getState().exportPausing).toEqual([]);
  // A removed job drops its photos.
  send("export-queue-changed", { jobs: [], held: false });
  expect(useProcessStore.getState().exportItems.j).toBeUndefined();
  exportQueue = [];
});

it("reads a job's photos again when it ends while its first read is still on its way", async () => {
  const { invoke } = await import("@tauri-apps/api/core");
  const photo = (index: number, state: string) => ({
    index, path: `/p/${index}.RAF`, state, output: null, error: null, attempts: 0, startedMs: null, finishedMs: null,
  });
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  await vi.waitFor(() => expect(callbacks.has("export-item-progress")).toBe(true));
  const send = (name: string, payload: unknown) => act(() => callbacks.get(name)!({ payload }));
  const job = { id: "k", label: "1 photo as JPG", createdAt: "", total: 1, done: 0, failed: 0, failures: [] };
  let answerFirst: (items: unknown) => void = () => {};
  vi.mocked(invoke).mockImplementationOnce(() => new Promise((resolve) => { answerFirst = resolve; }));
  send("export-job-started", { id: "k", current: 0, total: 1 });
  send("export-queue-changed", { jobs: [{ ...job, state: "running" }], held: false });
  // Cancelled before the first read answers: the queue put the photo back.
  exportQueue = [photo(0, "pending")];
  send("export-queue-changed", { jobs: [{ ...job, state: "cancelled" }], held: false });
  await vi.waitFor(() => expect(useProcessStore.getState().exportItems.k?.[0].state).toBe("pending"));
  await act(async () => answerFirst([photo(0, "running")]));
  expect(useProcessStore.getState().exportItems.k[0].state).toBe("pending");
  exportQueue = [];
});

it("reads the list again and recounts offline photographs when the catalog caught up on a failed sync", async () => {
  const { invoke } = await import("@tauri-apps/api/core");
  const refreshImageList = vi.fn();
  useLibraryStore.setState({ sessionDirectory: "/job" });
  const counted = vi.fn();
  window.addEventListener("rawtonic:offline-counted", counted);
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList, markGenerated: vi.fn(),
  }));
  await vi.waitFor(() => expect(callbacks.has("file-operations-caught-up")).toBe(true));
  act(() => callbacks.get("file-operations-caught-up")!({ payload: ["Rename 1 photo"] }));
  expect(refreshImageList).toHaveBeenCalled();
  await vi.waitFor(() => expect(counted).toHaveBeenCalled());
  expect(invoke).toHaveBeenCalledWith("session_offline", { directory: "/job" });
  window.removeEventListener("rawtonic:offline-counted", counted);
  useLibraryStore.setState({ sessionDirectory: null });
});

it("tells Previous Import about each new hot folder capture", async () => {
  const imported = vi.fn();
  window.addEventListener("rawtonic:imports-changed", imported);
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  await vi.waitFor(() => expect(callbacks.has("hot-folder-imported")).toBe(true));
  const capture = (payload: object) =>
    act(() => callbacks.get("hot-folder-imported")!({
      payload: { destination: "/job/Capture/A.ARW", skipped: false, error: null, ...payload },
    }));
  capture({ skipped: true });
  capture({ error: "Disk full" });
  expect(imported).not.toHaveBeenCalled();
  capture({});
  expect(imported).toHaveBeenCalledTimes(1);
  window.removeEventListener("rawtonic:imports-changed", imported);
});

it("takes merge events only from the run the dialog waits for", () => {
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  useUIStore.getState().setUI({
    mergeModalState: { ...CLOSED_MERGE_MODAL, isOpen: true, isProcessing: true, job: 2 },
  });
  const complete = (job: number, base64: string) => act(() => {
    callbacks.get("merge-complete")!({ payload: { job, base64, overlay: null, report: {}, width: 4, height: 3 } });
  });
  act(() => callbacks.get("merge-progress")!({ payload: { job: 1, message: "Aligning" } }));
  complete(1, "stale");
  act(() => callbacks.get("merge-error")!({ payload: { job: 1, error: "failed" } }));
  expect(useUIStore.getState().mergeModalState).toMatchObject({
    isProcessing: true, previewBase64: null, progressMessage: null, error: null,
  });
  act(() => callbacks.get("merge-progress")!({ payload: { job: 2, message: "Merging" } }));
  expect(useUIStore.getState().mergeModalState.progressMessage).toBe("Merging");
  complete(2, "current");
  expect(useUIStore.getState().mergeModalState).toMatchObject({ isProcessing: false, previewBase64: "current" });
  useUIStore.getState().setUI({ mergeModalState: CLOSED_MERGE_MODAL });
});

it('shows ingested photographs in the grid as they arrive, listing the folder again at most four times a second', async () => {
  vi.useFakeTimers();
  const refreshImageList = vi.fn(async () => {});
  const refreshAllFolderTrees = vi.fn();
  useLibraryStore.getState().setLibrary({ currentFolderPath: '/jobs/Acme/Capture' });
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees, handleSelectSubfolder: vi.fn(), refreshImageList, markGenerated: vi.fn(),
  }));
  const imported = (destination: string, extra: Record<string, unknown> = {}) =>
    callbacks.get('session-ingest-imported')!({
      payload: { destination, skipped: false, error: null, ...extra },
    });
  // The first photograph appears at once.
  imported('/jobs/Acme/Capture/A.ARW');
  await act(() => vi.advanceTimersByTimeAsync(0));
  expect(refreshImageList).toHaveBeenCalledTimes(1);
  // 40 more within a second: the grid is listed again at most four times.
  for (let i = 0; i < 40; i++) {
    imported(`/jobs/Acme/Capture/B${i}.ARW`);
    await act(() => vi.advanceTimersByTimeAsync(25));
  }
  expect(refreshImageList.mock.calls.length).toBeLessThanOrEqual(5);
  expect(refreshImageList.mock.calls.length).toBeGreaterThanOrEqual(4);
  // The last one is shown too, and the folder counts follow (less often).
  await act(() => vi.advanceTimersByTimeAsync(2000));
  const listed = refreshImageList.mock.calls.length;
  expect(listed).toBeLessThanOrEqual(6);
  expect(refreshAllFolderTrees).toHaveBeenCalled();
  expect(refreshAllFolderTrees.mock.calls.length).toBeLessThanOrEqual(3);
  // Skipped duplicates, failures and photographs for another folder leave the grid as it is.
  imported('/jobs/Acme/Capture/C.ARW', { skipped: true });
  imported('/jobs/Acme/Capture/D.ARW', { error: 'disk full' });
  imported('/jobs/Acme/Selects/E.ARW');
  await act(() => vi.advanceTimersByTimeAsync(2000));
  expect(refreshImageList).toHaveBeenCalledTimes(listed);
});

it("shows a tethered capture at once: selected in the Library, opened when Develop is showing", async () => {
  const handleImageSelect = vi.fn();
  const refreshImageList = vi.fn(async () => {});
  useLibraryStore.getState().setLibrary({ currentFolderPath: "/job/Capture", multiSelectedPaths: [] });
  useUIStore.getState().setUI({ activeView: "library" });
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList, markGenerated: vi.fn(), handleImageSelect,
  }));
  await vi.waitFor(() => expect(callbacks.has("tether-captured")).toBe(true));
  const capture = (destination: string, extra: object = {}) =>
    act(() => callbacks.get("tether-captured")!({
      payload: { destination, skipped: false, error: null, ...extra },
    }));
  capture("/job/Capture/A.ARW");
  await vi.waitFor(() => expect(useLibraryStore.getState().libraryActivePath).toBe("/job/Capture/A.ARW"));
  expect(handleImageSelect).not.toHaveBeenCalled();
  useUIStore.getState().setUI({ activeView: "editor" });
  capture("/job/Capture/B.ARW");
  await vi.waitFor(() => expect(handleImageSelect).toHaveBeenCalledWith("/job/Capture/B.ARW", true));
  capture("/job/Capture/C.ARW", { error: "Disk full" });
  await Promise.resolve();
  expect(handleImageSelect).toHaveBeenCalledTimes(1);
  // Another folder showing: the capture's folder is shown first, and
  // Develop stays open (the real navigation leaves it unless asked not to).
  const handleSelectSubfolder = vi.fn(
    (path: string, _root?: boolean, _images?: unknown, _expand?: boolean, preserveEditor?: boolean) => {
      if (!preserveEditor) useUIStore.getState().setUI({ activeView: "library" });
      useLibraryStore.getState().setLibrary({ currentFolderPath: path });
    },
  );
  cleanup();
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder,
    refreshImageList, markGenerated: vi.fn(), handleImageSelect,
  }));
  useLibraryStore.getState().setLibrary({ currentFolderPath: "/job/Selects" });
  capture("/job/Capture/D.ARW");
  await vi.waitFor(() => expect(handleImageSelect).toHaveBeenCalledWith("/job/Capture/D.ARW", true));
  expect(handleSelectSubfolder).toHaveBeenCalledWith("/job/Capture", false, undefined, true, true);
  expect(useUIStore.getState().activeView).toBe("editor");
  // A capture of the library left behind (still arriving after a switch)
  // does not pull this library's view back.
  useLibraryStore.getState().setLibrary({ sessionDirectory: "/other", currentFolderPath: "/other/Capture" });
  handleSelectSubfolder.mockClear();
  handleImageSelect.mockClear();
  capture("/job/Capture/E.ARW", { session: "/job" });
  await Promise.resolve();
  expect(handleSelectSubfolder).not.toHaveBeenCalled();
  expect(handleImageSelect).not.toHaveBeenCalled();
  // Two captures in another folder: the slower navigation of the first
  // does not bring it back over the second.
  useLibraryStore.getState().setLibrary({ sessionDirectory: "/job", currentFolderPath: "/job/Selects" });
  handleImageSelect.mockClear();
  let finishFirst: () => void = () => {};
  handleSelectSubfolder.mockImplementationOnce((path: string) => new Promise<void>((resolve) => {
    finishFirst = () => {
      useLibraryStore.getState().setLibrary({ currentFolderPath: path });
      resolve();
    };
  }) as never);
  capture("/job/Capture/F.ARW", { session: "/job" });
  useLibraryStore.getState().setLibrary({ currentFolderPath: "/job/Selects" });
  capture("/job/Capture/G.ARW", { session: "/job" });
  await vi.waitFor(() => expect(handleImageSelect).toHaveBeenCalledWith("/job/Capture/G.ARW", true));
  finishFirst();
  await Promise.resolve();
  await Promise.resolve();
  expect(handleImageSelect).not.toHaveBeenCalledWith("/job/Capture/F.ARW", true);
  useLibraryStore.getState().setLibrary({ sessionDirectory: null });
  useUIStore.getState().setUI({ activeView: "library" });
});

it("says tether failures app-wide, with no tether panel shown", async () => {
  const { notify: toast } = await import("../activity/notify");
  const error = vi.spyOn(toast, "error").mockImplementation(() => 0 as never);
  const warning = vi.spyOn(toast, "warning").mockImplementation(() => 0 as never);
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(async () => {}), markGenerated: vi.fn(), handleImageSelect: vi.fn(),
  }));
  await vi.waitFor(() => expect(callbacks.has("tether-capture-failed")).toBe(true));
  act(() => callbacks.get("tether-capture-failed")!({ payload: { message: "The camera did not deliver the photo." } }));
  act(() => callbacks.get("tether-captured")!({
    payload: { destination: null, skipped: false, error: "Disk full", cameraName: "DSC00001.ARW", session: "/job" },
  }));
  act(() => callbacks.get("tether-incomplete")!({ payload: "/job/.tether/incomplete/00000001/DSC00002.ARW" }));
  // One message each (the texts come from the locale files).
  expect(error).toHaveBeenCalledTimes(2);
  expect(warning).toHaveBeenCalledTimes(1);
  error.mockRestore();
  warning.mockRestore();
});

it('decodes an Edit With render again when another program saved it while it is shown', () => {
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(), handleImageSelect: vi.fn(),
  }));
  const saved = (path: string) => act(() => callbacks.get('edit-with-saved')!({ payload: { path } }));
  const shown = { path: '/s/A_edit.tiff', isReady: true, isDecoded: true, width: 10 } as SelectedImage;
  useEditorStore.getState().setEditor({ selectedImage: shown });
  saved('/s/B_edit.tiff');
  expect(useEditorStore.getState().selectedImage).toBe(shown);
  // During a slider drag the recipe is still a draft: nothing reloads yet.
  useEditorStore.getState().setEditor({ isSliderDragging: true, adjustments: { exposure: 0.5 } as never });
  saved('/s/A_edit.tiff');
  expect(useEditorStore.getState().selectedImage).toBe(shown);
  useEditorStore.getState().setEditor({ adjustments: { exposure: 0.7 } as never });
  act(() => useEditorStore.getState().setEditor({ isSliderDragging: false }));
  // The recipe in the editor (the drag's last value) is saved, then the
  // render is decoded again. That save makes no thumbnail: the decode
  // still loaded is the render before the other program's save.
  expect(vi.mocked(invoke)).toHaveBeenCalledWith('save_metadata_and_update_thumbnail', {
    path: '/s/A_edit.tiff',
    adjustments: { exposure: 0.7 },
    updateThumbnail: false,
  });
  expect(useEditorStore.getState().selectedImage).toMatchObject({
    path: '/s/A_edit.tiff', isDecoded: false, isReady: false, width: 10,
  });
});

it('keeps the undo history of an Edit With render decoded again after a save', () => {
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  const steps = [{ exposure: 0 }, { exposure: 0.3 }, { exposure: 0.6 }] as never[];
  useEditorStore.getState().setEditor({
    isSliderDragging: false,
    selectedImage: { path: '/s/A_edit.tiff', isReady: true, isDecoded: true } as SelectedImage,
    adjustments: steps[1],
    history: steps,
    historyIndex: 1,
  });
  act(() => callbacks.get('edit-with-saved')!({ payload: { path: '/s/A_edit.tiff' } }));
  // The loader starts a new history for the decoded source…
  act(() => useEditorStore.getState().setEditor({ history: [steps[1]], historyIndex: 0 }));
  act(() => useEditorStore.getState().setEditor({
    selectedImage: { path: '/s/A_edit.tiff', isReady: true, isDecoded: true } as SelectedImage,
  }));
  // …and the kept one comes back: undo and redo still work.
  expect(useEditorStore.getState().history).toBe(steps);
  expect(useEditorStore.getState().historyIndex).toBe(1);
});

it('keeps an undo step still pending when an Edit With render is decoded again', () => {
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  const steps = [{ exposure: 0 }, { exposure: 0.3 }] as never[];
  const edit = { exposure: 0.9 } as never;
  useEditorStore.getState().setEditor({
    isSliderDragging: false,
    selectedImage: { path: '/s/A_edit.tiff', isReady: true, isDecoded: true } as SelectedImage,
    adjustments: edit,
    history: steps,
    historyIndex: 1,
  });
  // The last edit's undo step waits for its debounce.
  debouncedSetHistory(edit);
  act(() => callbacks.get('edit-with-saved')!({ payload: { path: '/s/A_edit.tiff' } }));
  // Its timer fires while the render decodes.
  act(() => debouncedSetHistory.flush());
  act(() => useEditorStore.getState().setEditor({ history: [edit], historyIndex: 0 }));
  act(() => useEditorStore.getState().setEditor({
    selectedImage: { path: '/s/A_edit.tiff', isReady: true, isDecoded: true } as SelectedImage,
  }));
  const { history, historyIndex, adjustments } = useEditorStore.getState();
  expect(history.slice(0, 2)).toEqual(steps);
  expect(history[historyIndex]).toEqual(adjustments);
  expect(historyIndex).toBe(2);
});
