import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useBrowserViewPreview } from "./useBrowserViewPreview";
import { Invokes } from "../components/ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
let complete: ((value: ArrayBuffer) => void)[];
const jpeg = () => {
  const buffer = new ArrayBuffer(12);
  const view = new DataView(buffer);
  view.setUint32(0, 6000, true);
  view.setUint32(4, 4000, true);
  return buffer;
};
beforeEach(() => {
  vi.useFakeTimers();
  complete = [];
  vi.mocked(invoke).mockImplementation(
    () => new Promise((resolve) => complete.push(resolve)),
  );
  let sequence = 0;
  vi.stubGlobal("URL", {
    createObjectURL: vi.fn(() => `blob:cell-${++sequence}`),
    revokeObjectURL: vi.fn(),
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});
const tick = () => act(() => vi.advanceTimersByTime(100));
const finish = (index: number) =>
  act(async () => {
    complete[index](jpeg());
  });

it("isolates secondary recipes, ignores a stale photo and releases its own URLs", async () => {
  const { result, rerender, unmount } = renderHook(
    ({ path }) => useBrowserViewPreview(path, null, 1024),
    { initialProps: { path: "/first.raw" } },
  );
  tick();
  expect(invoke).toHaveBeenCalledWith(Invokes.RenderBrowserView, {
    path: "/first.raw",
    jsAdjustments: null,
    targetResolution: 1024,
  });
  rerender({ path: "/next.raw" });
  tick();
  await finish(1);
  expect(result.current.frame).toMatchObject({
    url: "blob:cell-1",
    width: 6000,
    height: 4000,
  });
  await finish(0);
  expect(result.current.frame?.url).toBe("blob:cell-1");
  expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
  unmount();
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:cell-1");
});

it("keeps a fit frame during zoom refinement but hides a superseded recipe", async () => {
  const { result, rerender } = renderHook(
    ({ recipe, resolution }) =>
      useBrowserViewPreview("/photo.raw", recipe, resolution),
    { initialProps: { recipe: '{"exposure":0}', resolution: 1024 } },
  );
  tick();
  await finish(0);
  rerender({ recipe: '{"exposure":0}', resolution: 2048 });
  expect(result.current.frame?.url).toBe("blob:cell-1");
  tick();
  await finish(1);
  expect(result.current.frame?.url).toBe("blob:cell-2");
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:cell-1");
  rerender({ recipe: '{"exposure":1}', resolution: 2048 });
  expect(result.current.frame).toBeNull();
});
