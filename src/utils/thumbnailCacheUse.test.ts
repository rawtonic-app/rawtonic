import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { noteThumbnailUse } from "./thumbnailCacheUse";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn().mockResolvedValue(undefined),
}));
const frames: FrameRequestCallback[] = [];
function setup() {
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    frames.push(callback);
    return frames.length;
  });
}
afterEach(() => {
  while (frames.length) frames.shift()!(0);
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

it("batches actual small/medium reads by cache family, excluding other data", () => {
  setup();
  const key = "b".repeat(64);
  noteThumbnailUse(`asset:///cache/${key}_small.jpg`);
  noteThumbnailUse(
    `https://asset.localhost/cache/${key}_medium.jpg?recovered=1`,
  );
  noteThumbnailUse(`https://example.org/${"c".repeat(64)}_small.jpg`);
  noteThumbnailUse("data:image/jpeg;base64,AA");
  noteThumbnailUse("blob:frame");
  noteThumbnailUse("asset:///photos/original.RAW");
  expect(invoke).not.toHaveBeenCalled();
  frames.shift()!(0);
  expect(invoke).toHaveBeenCalledExactlyOnceWith("storage_touch_thumbnails", {
    keys: [key],
  });
});

it("keeps each native touch request bounded and drains remaining reads", () => {
  setup();
  for (let index = 0; index < 501; index++)
    noteThumbnailUse(
      `asset:///cache/${index.toString(16).padStart(64, "0")}_small.jpg`,
    );
  frames.shift()!(0);
  expect(
    (vi.mocked(invoke).mock.calls[0][1] as { keys: string[] }).keys,
  ).toHaveLength(500);
  frames.shift()!(0);
  expect(
    (vi.mocked(invoke).mock.calls[1][1] as { keys: string[] }).keys,
  ).toHaveLength(1);
});
