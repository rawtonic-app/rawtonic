import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import StudioBrowser, { isEmptySpace } from "./StudioBrowser";
import { useLibraryStore } from "../../store/useLibraryStore";
import type { ImageFile } from "../ui/AppProperties";
import { clearLibrarySelection, reselectForDevelop } from "./workspaceInteractions";

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
