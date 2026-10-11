import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useTauriListeners } from "./useTauriListeners";
import { useProcessStore } from "../store/useProcessStore";
import { useLibraryStore } from "../store/useLibraryStore";
import { useEditorStore } from '../store/useEditorStore';
import { CLOSED_MERGE_MODAL, useUIStore } from '../store/useUIStore';
import type { ImageFile, SelectedImage } from '../components/ui/AppProperties';
const callbacks = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name, callback) => {
    callbacks.set(name, callback);
    return Promise.resolve(() => {});
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => `asset://${path}`,
  invoke: vi.fn(() => Promise.resolve(exportQueue)),
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

it("loads the export queue at start and points to interrupted exports", async () => {
  vi.useRealTimers();
  const { toast } = await import("react-toastify");
  const info = vi.spyOn(toast, "info").mockImplementation(() => 0 as never);
  exportQueue = [{ id: "j", label: "3 photos as JPG", state: "interrupted", createdAt: "", total: 3, done: 1, failed: 0 }];
  renderHook(() => useTauriListeners({
    refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
    refreshImageList: vi.fn(), markGenerated: vi.fn(),
  }));
  await vi.waitFor(() => expect(useProcessStore.getState().exportQueue).toHaveLength(1));
  expect(info).toHaveBeenCalledWith("An export was interrupted (1 of 3 done). Resume it in Export.");
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
