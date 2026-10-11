import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, renderHook } from "@testing-library/react";
import {
  DROP_TARGET_CLASS,
  FILE_DRAG_CLASS,
  dropTargetOf,
  folderAcceptsDrop,
  toCssPoint,
  useWindowFileDrop,
} from "./fileDrop";

type DragDropPayload =
  | { type: "enter"; paths: string[]; position: { x: number; y: number } }
  | { type: "over"; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };
const drag = vi.hoisted(() => ({ handler: null as null | ((event: { payload: DragDropPayload }) => void) }));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => true }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async (handler: (event: { payload: DragDropPayload }) => void) => {
      drag.handler = handler;
      return () => {
        drag.handler = null;
      };
    },
  }),
}));

let album: HTMLElement;
let folder: HTMLElement;
let elsewhere: HTMLElement;
beforeEach(() => {
  document.body.innerHTML = `
    <div id="list">
      <button data-album-drop="a1" data-album-name="Selects 2026"><span id="album-label">Selects 2026</span></button>
      <div data-folder-drop="/jobs/Acme/Capture/Day 2"><button><span id="folder-label">Day 2</span></button></div>
      <p id="elsewhere">grid</p>
    </div>`;
  album = document.getElementById("album-label")!;
  folder = document.getElementById("folder-label")!;
  elsewhere = document.getElementById("elsewhere")!;
});
afterEach(() => {
  cleanup();
  document.body.className = "";
});

describe("drop targets", () => {
  it("finds the album or folder a point is over", () => {
    expect(dropTargetOf(album)?.target).toEqual({ kind: "album", id: "a1", name: "Selects 2026" });
    expect(dropTargetOf(folder)?.target).toEqual({ kind: "folder", path: "/jobs/Acme/Capture/Day 2" });
    expect(dropTargetOf(elsewhere)).toBeNull();
    expect(dropTargetOf(null)).toBeNull();
  });

  it("converts the webview's physical drag position to CSS pixels", () => {
    expect(toCssPoint({ x: 400, y: 300 }, 2)).toEqual({ x: 200, y: 150 });
  });

  it("accepts only folders inside the open library, not its Trash, own or hidden folders", () => {
    const library = "/jobs/Acme";
    expect(folderAcceptsDrop("/jobs/Acme/Capture", library)).toBe(true);
    expect(folderAcceptsDrop("/jobs/Acme/Capture/Day 2", library)).toBe(true);
    expect(folderAcceptsDrop("/jobs/Acme/Selects", "/jobs/Acme/")).toBe(true);
    expect(folderAcceptsDrop("/jobs/Acme", library)).toBe(false);
    expect(folderAcceptsDrop("/jobs/Acme Other/Capture", library)).toBe(false);
    expect(folderAcceptsDrop("/elsewhere/Photos", library)).toBe(false);
    expect(folderAcceptsDrop("/jobs/Acme/Trash", library)).toBe(false);
    expect(folderAcceptsDrop("/jobs/Acme/masks", library)).toBe(false);
    expect(folderAcceptsDrop("/jobs/Acme/.cache/x", library)).toBe(false);
    expect(folderAcceptsDrop("/jobs/Acme/Capture", null)).toBe(false);
    expect(folderAcceptsDrop("C:\\Jobs\\Acme\\Capture", "C:\\Jobs\\Acme")).toBe(true);
  });
});

describe("files dragged over the window", () => {
  it("highlights the window and the album under the pointer, and reports the drop with its target", async () => {
    const onDrop = vi.fn();
    const onDrag = vi.fn();
    const at = vi.fn((x: number) => (x < 100 ? album : x < 200 ? folder : elsewhere));
    document.elementFromPoint = at as unknown as typeof document.elementFromPoint;
    renderHook(() => useWindowFileDrop(onDrop, onDrag));
    await vi.waitFor(() => expect(drag.handler).not.toBeNull());
    const albumRow = album.closest("button")!;
    const folderRow = folder.closest("[data-folder-drop]")!;

    drag.handler!({ payload: { type: "enter", paths: ["/card/DCIM"], position: { x: 300, y: 10 } } });
    expect(document.body.classList.contains(FILE_DRAG_CLASS)).toBe(true);
    expect(onDrag).toHaveBeenLastCalledWith(true);
    expect(document.querySelector(`.${DROP_TARGET_CLASS}`)).toBeNull();

    // Physical pixels: with a device pixel ratio of 1, x 50 is over the album.
    drag.handler!({ payload: { type: "over", position: { x: 50, y: 10 } } });
    expect(albumRow.classList.contains(DROP_TARGET_CLASS)).toBe(true);
    drag.handler!({ payload: { type: "over", position: { x: 150, y: 10 } } });
    expect(albumRow.classList.contains(DROP_TARGET_CLASS)).toBe(false);
    expect(folderRow.classList.contains(DROP_TARGET_CLASS)).toBe(true);

    drag.handler!({ payload: { type: "drop", paths: ["/card/DCIM", "/card/A.CR3"], position: { x: 50, y: 10 } } });
    expect(onDrop).toHaveBeenCalledWith({
      paths: ["/card/DCIM", "/card/A.CR3"],
      target: { kind: "album", id: "a1", name: "Selects 2026" },
    });
    expect(document.body.classList.contains(FILE_DRAG_CLASS)).toBe(false);
    expect(document.querySelector(`.${DROP_TARGET_CLASS}`)).toBeNull();
    expect(onDrag).toHaveBeenLastCalledWith(false);
  });

  it("reports a drop elsewhere without a target, and nothing for a drag that leaves", async () => {
    const onDrop = vi.fn();
    document.elementFromPoint = (() => elsewhere) as unknown as typeof document.elementFromPoint;
    renderHook(() => useWindowFileDrop(onDrop));
    await vi.waitFor(() => expect(drag.handler).not.toBeNull());
    drag.handler!({ payload: { type: "enter", paths: ["/a.nef"], position: { x: 1, y: 1 } } });
    drag.handler!({ payload: { type: "leave" } });
    expect(onDrop).not.toHaveBeenCalled();
    expect(document.body.classList.contains(FILE_DRAG_CLASS)).toBe(false);
    drag.handler!({ payload: { type: "drop", paths: ["/a.nef"], position: { x: 1, y: 1 } } });
    expect(onDrop).toHaveBeenCalledWith({ paths: ["/a.nef"], target: null });
  });
});
