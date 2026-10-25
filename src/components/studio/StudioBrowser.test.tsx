import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { useProcessStore } from "../../store/useProcessStore";
import StudioBrowser, { isEmptySpace } from "./StudioBrowser";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useUIStore } from "../../store/useUIStore";
import type { ImageFile } from "../ui/AppProperties";
import { clearLibrarySelection, reselectForDevelop } from "./workspaceInteractions";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), convertFileSrc: (path: string) => `asset://${path}` }));

const file = (path: string): ImageFile => ({
  path,
  is_edited: false,
  modified: 0,
  rating: 0,
  tags: null,
  exif: null,
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: true,
  group_id: null,
});
const files = ["/s/a.NEF", "/s/b.NEF", "/s/c.NEF"].map(file);

/** The grid's client area: 800 × 500 with a 12 px scroll bar beside and below. */
function sizeBrowser(el: HTMLElement) {
  Object.defineProperty(el, "clientWidth", { value: 800, configurable: true });
  Object.defineProperty(el, "clientHeight", { value: 500, configurable: true });
  el.getBoundingClientRect = () =>
    ({ left: 0, top: 0, width: 812, height: 512, right: 812, bottom: 512, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
}

function grid(onClearSelection = vi.fn(), film = false) {
  const view = render(
    <StudioBrowser
      files={files}
      film={film}
      currentPath="/s/b.NEF"
      onSelect={vi.fn()}
      onOpen={vi.fn()}
      onRate={vi.fn()}
      onContextMenu={vi.fn()}
      requestThumbnails={vi.fn()}
      onClearSelection={film ? undefined : onClearSelection}
    />,
  );
  const browser = view.container.querySelector<HTMLElement>(".studio-browser")!;
  sizeBrowser(browser);
  return { ...view, browser, onClearSelection };
}

const click = (target: Element, x: number, y: number, up: { x: number; y: number } = { x, y }, init = {}) => {
  fireEvent.pointerDown(target, { button: 0, clientX: x, clientY: y, ...init });
  fireEvent.pointerUp(target, { button: 0, clientX: up.x, clientY: up.y, ...init });
};

beforeEach(() => {
  useProcessStore.setState({ thumbnails: {}, mediumThumbnails: {} });
  vi.mocked(invoke).mockReset();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  useLibraryStore.setState({ multiSelectedPaths: ["/s/b.NEF"], imageRatings: {}, imageFlags: {} });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("lays the grid out for its real width from the first frame, not for a guessed one", () => {
  // The client area exists before the grid mounts (the Library coming back
  // from Develop); the resize observer reports only after the first paint.
  const width = vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(1440);
  const height = vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(800);
  try {
    grid();
    expect(useUIStore.getState().gridColumns).toBe(Math.floor(1440 / 190));
  } finally {
    width.mockRestore();
    height.mockRestore();
  }
});

describe("library grid empty space", () => {
  it("clears the selection on a click on the grid's background", () => {
    const { browser, onClearSelection } = grid();
    click(browser, 400, 450);
    expect(onClearSelection).toHaveBeenCalledTimes(1);
  });

  it("clears it from the gap around a photograph too, not from a photograph", () => {
    const { browser, onClearSelection, container } = grid();
    const tile = container.querySelector(".studio-photo")!;
    click(tile, 50, 50);
    click(tile.querySelector("button")!, 50, 50);
    expect(onClearSelection).not.toHaveBeenCalled();
    // The cell's padding around the tile is empty space.
    click(tile.parentElement!, 4, 4);
    expect(onClearSelection).toHaveBeenCalledTimes(1);
    expect(browser.contains(tile)).toBe(true);
  });

  it("does not clear it from a scroll bar, a drag or a modified click", () => {
    const { browser, onClearSelection } = grid();
    click(browser, 806, 200); // vertical scroll bar
    click(browser, 300, 506); // horizontal scroll bar
    click(browser, 300, 300, { x: 360, y: 340 }); // a drag (room for a marquee)
    click(browser, 300, 300, undefined, { shiftKey: true });
    click(browser, 300, 300, undefined, { metaKey: true });
    fireEvent.pointerDown(browser, { button: 2, clientX: 300, clientY: 300 });
    fireEvent.pointerUp(browser, { button: 2, clientX: 300, clientY: 300 });
    expect(onClearSelection).not.toHaveBeenCalled();
  });

  it("is not offered in the filmstrip", () => {
    const { browser, onClearSelection } = grid(vi.fn(), true);
    click(browser, 400, 450);
    expect(onClearSelection).not.toHaveBeenCalled();
  });

  it("marks the open photograph in the grid only while it is selected", () => {
    const { container, rerender } = grid();
    expect(container.querySelector(".is-current")?.getAttribute("aria-label")).toBe("b.NEF");
    useLibraryStore.setState({ multiSelectedPaths: [] });
    rerender(
      <StudioBrowser
        files={files}
        currentPath="/s/b.NEF"
        onSelect={vi.fn()}
        onOpen={vi.fn()}
        onRate={vi.fn()}
        onContextMenu={vi.fn()}
        requestThumbnails={vi.fn()}
      />,
    );
    expect(container.querySelector(".is-current")).toBeNull();
  });

  it("knows a photograph from empty space", () => {
    const { browser, container } = grid();
    expect(isEmptySpace(browser, browser, 10, 10)).toBe(true);
    expect(isEmptySpace(browser, container.querySelector(".studio-photo-open"), 10, 10)).toBe(false);
    expect(isEmptySpace(browser, document.body, 10, 10)).toBe(false);
  });
});

describe("emptied selection", () => {
  it("clears the selection but keeps the open photograph and the keyboard focus", () => {
    useLibraryStore.setState({
      multiSelectedPaths: ["/s/a.NEF", "/s/b.NEF"],
      libraryActivePath: "/s/b.NEF",
      selectionAnchorPath: "/s/a.NEF",
    });
    clearLibrarySelection();
    const library = useLibraryStore.getState();
    expect(library.multiSelectedPaths).toEqual([]);
    expect(library.selectionAnchorPath).toBeNull();
    expect(library.libraryActivePath).toBe("/s/b.NEF");
  });

  it("selects the last primary again when Develop opens on it", () => {
    useLibraryStore.setState({ multiSelectedPaths: [], libraryActivePath: "/s/c.NEF" });
    reselectForDevelop("/s/b.NEF");
    expect(useLibraryStore.getState().multiSelectedPaths).toEqual(["/s/b.NEF"]);
    expect(useLibraryStore.getState().libraryActivePath).toBe("/s/b.NEF");
    // A selection that is there stays as it is (the multi view shows it).
    useLibraryStore.setState({ multiSelectedPaths: ["/s/a.NEF", "/s/c.NEF"] });
    reselectForDevelop("/s/b.NEF");
    expect(useLibraryStore.getState().multiSelectedPaths).toEqual(["/s/a.NEF", "/s/c.NEF"]);
  });
});

describe("filmstrip wheel", () => {
  function strip(film = true) {
    const view = grid(vi.fn(), film);
    Object.defineProperties(view.browser, {
      scrollWidth: { value: 2400, configurable: true },
      scrollHeight: { value: 500, configurable: true },
    });
    view.browser.scrollLeft = 200;
    view.browser.style.lineHeight = "20px";
    return view;
  }
  function wheel(target: Element, init: WheelEventInit = {}) {
    const event = new WheelEvent("wheel", { bubbles: true, cancelable: true, deltaY: 40, ...init });
    fireEvent(target, event);
    return event;
  }

  it.each([
    [{ deltaY: 40 }, 240],
    [{ deltaY: -30 }, 170],
    [{ deltaY: 2.5, deltaX: 1 }, 202.5],
    [{ deltaY: 3, deltaMode: 1 }, 260],
    [{ deltaY: 1, deltaMode: 2 }, 1000],
    [{ deltaY: -1000 }, 0],
    [{ deltaY: 3000 }, 1600],
  ])("maps vertical input to horizontal pixels: %j", (init, expected) => {
    const { browser } = strip();
    expect(wheel(browser.querySelector("button")!, init).defaultPrevented).toBe(true);
    expect(browser.scrollLeft).toBe(expected);
  });

  it.each([
    { deltaX: 50 }, { deltaX: 40 }, { deltaY: 0 },
    { shiftKey: true }, { ctrlKey: true }, { metaKey: true }, { cancelable: false },
  ])("preserves native gestures: %j", (init) => {
    const { browser } = strip();
    expect(wheel(browser, init).defaultPrevented).toBe(false);
    expect(browser.scrollLeft).toBe(200);
  });

  it("leaves a vertically scrolling strip and a fitting strip native", () => {
    const { browser } = strip();
    Object.defineProperty(browser, "scrollHeight", { value: 600 });
    expect(wheel(browser).defaultPrevented).toBe(false);
    Object.defineProperty(browser, "scrollHeight", { value: 500 });
    Object.defineProperty(browser, "scrollWidth", { value: 800 });
    expect(wheel(browser).defaultPrevented).toBe(false);
    expect(browser.scrollLeft).toBe(200);
  });

  it("does not intercept the Library grid or another panel", () => {
    const { browser } = strip(false);
    expect(wheel(browser).defaultPrevented).toBe(false);
    expect(wheel(document.body).defaultPrevented).toBe(false);
    expect(browser.scrollLeft).toBe(200);
  });

  it("keeps wheel events outside the mounted filmstrip native", () => {
    const { browser } = strip();
    expect(wheel(document.body).defaultPrevented).toBe(false);
    expect(browser.scrollLeft).toBe(200);
  });

  it("uses the font size when line height is normal", () => {
    const { browser } = strip();
    browser.style.lineHeight = "normal";
    browser.style.fontSize = "10px";
    wheel(browser, { deltaY: 2, deltaMode: 1 });
    expect(browser.scrollLeft).toBe(224);
  });

  it("removes the native listener when the filmstrip unmounts", () => {
    const { browser, unmount } = strip();
    unmount();
    expect(wheel(browser).defaultPrevented).toBe(false);
    expect(browser.scrollLeft).toBe(200);
  });
});

it("recovers an evicted thumbnail when its Library tile becomes visible", async () => {
  vi.mocked(invoke).mockResolvedValue(["/cache/restored.jpg", "/cache/medium.jpg"]);
  useProcessStore.setState({ thumbnails: { "/s/a.NEF": "asset:///cache/evicted.jpg" } });
  const view = grid();
  fireEvent.error(view.container.querySelector("img")!);
  await waitFor(() => expect(view.container.querySelector("img")?.getAttribute("src")).toContain("restored.jpg?recovered="));
  expect(invoke).toHaveBeenCalledWith("restore_thumbnail", { path: "/s/a.NEF" });
  fireEvent.error(view.container.querySelector("img")!);
  await waitFor(() => expect(view.container.querySelector("img")).toBeNull());
  expect(invoke).toHaveBeenCalledTimes(1);
});
