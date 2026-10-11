import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useLibraryExif } from "./useLibraryExif";
import { useLibraryStore } from "../store/useLibraryStore";
import type { ImageFile } from "../components/ui/AppProperties";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
it("does no background RAW reads for ordinary browsing and discards a stale EXIF response", async () => {
  const images = Array.from({ length: 20596 }, (_, i) => ({
    path: `/image-${i}.RAF`,
    exif: null,
  })) as ImageFile[];
  useLibraryStore
    .getState()
    .setLibrary({
      imageList: images,
      currentFolderPath: "/first",
      activeAlbumId: null,
    });
  let resolve!: (value: Record<string, Record<string, string>>) => void;
  vi.mocked(invoke).mockImplementation(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const { rerender } = renderHook(({ required }) => useLibraryExif(required), {
    initialProps: { required: false },
  });
  expect(invoke).not.toHaveBeenCalled();
  rerender({ required: true });
  expect(invoke).toHaveBeenCalledTimes(1);
  expect((vi.mocked(invoke).mock.calls[0][1] as { paths: string[] }).paths).toHaveLength(100);
  rerender({ required: false });
  await act(async () => resolve({ "/image-0.RAF": { ISO: "100" } }));
  expect(useLibraryStore.getState().imageList).toBe(images);
  expect(invoke).toHaveBeenCalledTimes(1);
});
