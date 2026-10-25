import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useProcessStore } from "../../store/useProcessStore";
import CachedThumbnail from "./CachedThumbnail";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  convertFileSrc: (path: string) => `asset://${path}`,
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("regenerates an evicted cache file and retries with a fresh asset URL", async () => {
  vi.mocked(invoke).mockResolvedValue([
    "/cache/small.jpg",
    "/cache/medium.jpg",
  ]);
  const view = render(
    <CachedThumbnail
      sourcePath="/photos/a.RAW"
      src="asset:///cache/small.jpg"
      alt="photo"
    />,
  );
  fireEvent.error(view.getByAltText("photo"));
  await waitFor(() =>
    expect(view.getByAltText("photo").getAttribute("src")).toMatch(
      /^asset:\/\/\/cache\/small.jpg\?recovered=/,
    ),
  );
  expect(invoke).toHaveBeenCalledWith("restore_thumbnail", {
    path: "/photos/a.RAW",
  });
  expect(useProcessStore.getState().thumbnails["/photos/a.RAW"]).toBe(
    view.getByAltText("photo").getAttribute("src"),
  );
  // A missing/offline original or a second broken image never creates a retry loop.
  fireEvent.error(view.getByAltText("photo"));
  await act(async () => {});
  expect(invoke).toHaveBeenCalledTimes(1);
});

it("coalesces simultaneous grid/filmstrip recovery and ignores unmounted readers", async () => {
  let resolve!: (paths: string[]) => void;
  vi.mocked(invoke).mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  const first = render(
    <CachedThumbnail
      sourcePath="/photos/b.RAW"
      src="asset:///cache/b.jpg"
      alt="grid"
    />,
  );
  const second = render(
    <CachedThumbnail
      sourcePath="/photos/b.RAW"
      src="asset:///cache/b.jpg"
      alt="film"
    />,
  );
  fireEvent.error(first.getByAltText("grid"));
  fireEvent.error(second.getByAltText("film"));
  expect(invoke).toHaveBeenCalledTimes(1);
  first.unmount();
  await act(async () =>
    resolve(["/cache/restored.jpg", "/cache/restored-medium.jpg"]),
  );
  expect(second.getByAltText("film").getAttribute("src")).toContain(
    "restored.jpg?recovered=",
  );
});

it("shows a placeholder when regeneration fails without issuing repeated requests", async () => {
  vi.mocked(invoke).mockRejectedValue(new Error("offline"));
  const view = render(
    <CachedThumbnail
      sourcePath="/photos/offline.RAW"
      src="asset:///cache/missing.jpg"
      alt="offline"
    />,
  );
  fireEvent.error(view.getByAltText("offline"));
  await act(async () => {});
  expect(view.container.querySelector("img")).toBeNull();
  expect(invoke).toHaveBeenCalledTimes(1);
});

it("restores medium cache URLs with Windows path separators", async () => {
  vi.mocked(invoke).mockResolvedValue([
    "C:\\cache\\small.jpg",
    "C:\\cache\\medium.jpg",
  ]);
  const view = render(
    <CachedThumbnail
      sourcePath="C:\\photos\\a.RAW"
      medium
      src="asset://missing.jpg"
      alt="medium"
    />,
  );
  fireEvent.error(view.getByAltText("medium"));
  await waitFor(() =>
    expect(view.getByAltText("medium").getAttribute("src")).toMatch(
      /^asset:\/\/C:\/cache\/medium.jpg\?recovered=/,
    ),
  );
});

it("can recover a later eviction after an image has successfully loaded", async () => {
  vi.mocked(invoke).mockResolvedValue([
    "/cache/small.jpg",
    "/cache/medium.jpg",
  ]);
  const view = render(
    <CachedThumbnail
      sourcePath="/photos/again.RAW"
      src="asset:///cache/small.jpg"
      alt="again"
    />,
  );
  fireEvent.error(view.getByAltText("again"));
  await waitFor(() =>
    expect(view.getByAltText("again").getAttribute("src")).toContain(
      "?recovered=",
    ),
  );
  fireEvent.load(view.getByAltText("again"));
  fireEvent.error(view.getByAltText("again"));
  await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
});
