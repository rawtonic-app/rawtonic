import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, waitFor, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { useProcessStore } from "../../store/useProcessStore";
import StudioBrowser, { isEmptySpace } from "./StudioBrowser";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useUIStore } from "../../store/useUIStore";
import type { ImageFile } from "../ui/AppProperties";
import { clearLibrarySelection, ownsPanelShortcut, reselectForDevelop } from "./workspaceInteractions";

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

it("shows a readable label as well as its colour in the grid and filmstrip", () => {
  files[0].tags = ["color:green"];
  try {
    const view = grid();
    expect(view.getByTitle("Green label").textContent).toBe("Green");
    view.unmount();
    expect(grid(vi.fn(), true).getByTitle("Green label").textContent).toBe("Green");
  } finally { files[0].tags = null; }
});

it("offers import in an empty library and clear filters for an empty result", () => {
  const onImport = vi.fn();
  const props = { files: [], onSelect: vi.fn(), onOpen: vi.fn(), onRate: vi.fn(), onContextMenu: vi.fn(), requestThumbnails: vi.fn(), onImport };
  useLibraryStore.setState({ sessionDirectory: "/s", imageList: [], isViewLoading: false, filterCriteria: { colors: [], rating: 0, rawStatus: "all" as never }, searchCriteria: { text: "", tags: [], mode: "OR" } });
  const view = render(<StudioBrowser {...props} />);
  fireEvent.click(screen.getByRole("button", { name: "Import photos" }));
  expect(onImport).toHaveBeenCalledOnce();
  act(() => useLibraryStore.setState({ imageList: files, searchCriteria: { text: "missing", tags: [], mode: "OR" } }));
  fireEvent.click(screen.getByRole("button", { name: "Clear filters" }));
  expect(useLibraryStore.getState().searchCriteria.text).toBe("");
  view.unmount();
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
    expect(container.querySelector(".is-current [role=option]")?.getAttribute("aria-label")).toBe("b.NEF");
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

describe("library grid keyboard focus", () => {
  // The open photograph stands for the active one unless a test sets it.
  beforeEach(() => useLibraryStore.setState({ libraryActivePath: null }));
  const props = {
    files,
    onSelect: vi.fn(),
    onOpen: vi.fn(),
    onRate: vi.fn(),
    onContextMenu: vi.fn(),
    requestThumbnails: vi.fn(),
  };
  const tabStops = (container: HTMLElement) =>
    Array.from(container.querySelectorAll<HTMLElement>("button, [tabindex]")).filter((el) => el.tabIndex >= 0);

  it("has one Tab stop, on the current photograph, and says where each tile is", () => {
    const { container, getAllByRole } = render(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    const stops = tabStops(container);
    expect(stops).toHaveLength(1);
    expect(stops[0].getAttribute("aria-label")).toBe("b.NEF");
    const options = getAllByRole("option");
    expect(options.map((o) => [o.getAttribute("aria-posinset"), o.getAttribute("aria-setsize")])).toEqual([
      ["1", "3"],
      ["2", "3"],
      ["3", "3"],
    ]);
  });

  it("starts at the first photograph when none is current", () => {
    const { container } = render(<StudioBrowser {...props} />);
    expect(tabStops(container).map((el) => el.getAttribute("aria-label"))).toEqual(["a.NEF"]);
  });

  it("keeps the rating buttons out of the Tab order and says the rating in the tile's description", () => {
    useLibraryStore.setState({ imageRatings: { "/s/a.NEF": 3 }, imageFlags: { "/s/a.NEF": 1 } });
    const { getAllByRole } = render(<StudioBrowser {...props} currentPath="/s/a.NEF" />);
    const [first, second] = getAllByRole("option");
    const description = (el: HTMLElement) =>
      document.getElementById(el.getAttribute("aria-describedby") ?? "")?.textContent;
    expect(description(first)).toBe("3 stars, Picked");
    expect(description(second)).toBe("Unrated");
    const stars = first.closest(".studio-photo")!.querySelectorAll<HTMLButtonElement>(".studio-photo-rating button");
    expect(stars).toHaveLength(5);
    stars.forEach((star) => expect(star.tabIndex).toBe(-1));
    useLibraryStore.setState({ imageRatings: {}, imageFlags: {} });
  });

  it("moves the focus with the current photograph while a tile has it", () => {
    const { getAllByRole, rerender } = render(<StudioBrowser {...props} currentPath="/s/a.NEF" />);
    getAllByRole("option")[0].focus();
    rerender(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    expect(document.activeElement?.getAttribute("aria-label")).toBe("b.NEF");
    expect(getAllByRole("option")[1].tabIndex).toBe(0);
    expect(getAllByRole("option")[0].tabIndex).toBe(-1);
  });

  it("lets Tab hide the panels after a click, and leave the grid from a tile reached by the keyboard", () => {
    const { getAllByRole, rerender } = render(<StudioBrowser {...props} currentPath="/s/a.NEF" />);
    const [first] = getAllByRole("option");
    first.focus();
    expect(ownsPanelShortcut(document.activeElement)).toBe(false);
    first.blur();
    fireEvent.pointerDown(first);
    first.focus();
    expect(ownsPanelShortcut(document.activeElement)).toBe(true);
    rerender(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    expect(document.activeElement?.getAttribute("aria-label")).toBe("b.NEF");
    expect(ownsPanelShortcut(document.activeElement)).toBe(true);
    expect(ownsPanelShortcut(first)).toBe(false);
  });

  it("puts the Tab stop on the Library's active photograph after a modifier-click made it differ from the open one", () => {
    useLibraryStore.setState({ libraryActivePath: "/s/a.NEF" });
    const { container, getAllByRole } = render(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    expect(tabStops(container).map((el) => el.getAttribute("aria-label"))).toEqual(["a.NEF"]);
    act(() => getAllByRole("option")[0].focus());
    expect(document.activeElement?.getAttribute("aria-label")).toBe("a.NEF");
    act(() => useLibraryStore.setState({ libraryActivePath: "/s/c.NEF" }));
    expect(document.activeElement?.getAttribute("aria-label")).toBe("c.NEF");
    expect(tabStops(container).map((el) => el.getAttribute("aria-label"))).toEqual(["c.NEF"]);
    useLibraryStore.setState({ libraryActivePath: null });
  });

  it("makes the photograph entered by keyboard active in a folder that has none", () => {
    // A folder just opened: the listing arrives with no active photograph
    // (useAppNavigation publishes it that way). Tab presses the key, then
    // the browser moves the focus to the grid's first stop.
    useLibraryStore.setState({ libraryActivePath: null, multiSelectedPaths: [] });
    const { getByRole } = render(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    fireEvent.keyDown(document.body, { key: "Tab", code: "Tab" });
    // Tab reaches the grid's only Tab stop: the open photograph's tile.
    act(() => getByRole("option", { name: "b.NEF" }).focus());
    expect(useLibraryStore.getState()).toMatchObject({
      libraryActivePath: "/s/b.NEF",
      multiSelectedPaths: ["/s/b.NEF"],
    });
    // From here it is the grid's active photograph: the focus follows the
    // photographs picked elsewhere (that Tab does not hide the panels is
    // the keyboard entry the existing tests cover).
    act(() => useLibraryStore.setState({ libraryActivePath: "/s/c.NEF" }));
    expect(document.activeElement?.getAttribute("aria-label")).toBe("c.NEF");
    useLibraryStore.setState({ libraryActivePath: null });
  });

  it("takes no programmatic or clicked focus of a folder without an active photo as Tab entry", () => {
    // Not a Tab key (the focus put on a tile by code): nothing becomes active.
    useLibraryStore.setState({ libraryActivePath: null, multiSelectedPaths: [] });
    const { getAllByRole } = render(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    act(() => getAllByRole("option")[1].focus());
    expect(useLibraryStore.getState().libraryActivePath).toBeNull();
    // A click ends the keyboard entry path, even after a Tab.
    fireEvent.pointerDown(getAllByRole("option")[0]);
    fireEvent.keyDown(document.body, { key: "Tab", code: "Tab" });
    act(() => getAllByRole("option")[2].focus());
    expect(useLibraryStore.getState().libraryActivePath).toBeNull();
  });

  it("ends the Tab modality when the focus moves to a panel before a programmatic tile focus", () => {
    // Tab walked the panels before it ever came near the grid; a click in a
    // panel (focus outside the grid), then code puts the focus on a tile.
    // That focus is not a keyboard entry into the grid.
    useLibraryStore.setState({ libraryActivePath: null, multiSelectedPaths: [] });
    const { getAllByRole } = render(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    const panel = document.createElement("button");
    document.body.append(panel);
    fireEvent.keyDown(document.body, { key: "Tab", code: "Tab" });
    act(() => panel.focus());
    act(() => getAllByRole("option")[1].focus());
    expect(useLibraryStore.getState().libraryActivePath).toBeNull();
    panel.remove();
  });

  it("ends the Tab modality on a pointer press outside the grid", () => {
    // A click anywhere that is not the grid (here without moving widget
    // focus) makes the next focus no keyboard entry.
    useLibraryStore.setState({ libraryActivePath: null, multiSelectedPaths: [] });
    const { getAllByRole } = render(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    fireEvent.keyDown(document.body, { key: "Tab", code: "Tab" });
    fireEvent.pointerDown(document.body);
    act(() => getAllByRole("option")[1].focus());
    expect(useLibraryStore.getState().libraryActivePath).toBeNull();
  });

  it("makes the photograph reached by a real Tab press active in a folder that has no active one", () => {
    // A real Tab keydown on the panel before the grid, the browser's focus
    // traversal onto the grid's only Tab stop, and no click in between.
    useLibraryStore.setState({ libraryActivePath: null, multiSelectedPaths: [] });
    const { getAllByRole } = render(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    const panel = document.createElement("button");
    document.body.append(panel);
    act(() => panel.focus());
    fireEvent.keyDown(panel, { key: "Tab", code: "Tab" });
    act(() => getAllByRole("option")[1].focus());
    expect(useLibraryStore.getState().libraryActivePath).toBe("/s/b.NEF");
    panel.remove();
  });

  it("changes nothing when a photograph is picked by mouse in a folder that has no active one", () => {
    useLibraryStore.setState({ libraryActivePath: null });
    const { getAllByRole } = render(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    const [first] = getAllByRole("option");
    fireEvent.pointerDown(first);
    fireEvent.click(first);
    act(() => first.focus());
    expect(useLibraryStore.getState().libraryActivePath).toBeNull();
  });

  it("forgets a pending focus when the focus leaves the grid before the target's tile mounts", () => {
    // The active photograph is far above the grid's window: Tab onto the
    // stop in view scrolls to it and waits for its tile to mount.
    const many = Array.from({ length: 400 }, (_, n) => `/qa/photo-${n}.jpg`).map(file);
    Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 500 });
    useLibraryStore.setState({ libraryActivePath: "/qa/photo-0.jpg" });
    try {
      render(<StudioBrowser {...props} files={many} currentPath="/qa/photo-0.jpg" />);
      const grid = screen.getByRole("listbox");
      grid.scrollTop = 201 * 13;
      fireEvent.scroll(grid);
      const stop = Array.from(grid.querySelectorAll<HTMLElement>("[role=option]")).find((o) => o.tabIndex === 0)!;
      expect(stop.getAttribute("aria-label")).not.toBe("a.NEF");
      act(() => stop.focus());
      expect(grid.scrollTop).toBe(0);
      // A click in a panel takes the focus away before that tile shows.
      const panel = document.createElement("button");
      document.body.append(panel);
      act(() => panel.focus());
      expect(document.activeElement).toBe(panel);
      // Keys picked meanwhile and later never take the grid's focus back.
      act(() => useLibraryStore.setState({ libraryActivePath: many[1].path }));
      expect(document.activeElement).toBe(panel);
      act(() => useLibraryStore.setState({ libraryActivePath: many[2].path }));
      expect(document.activeElement).toBe(panel);
      panel.remove();
    } finally {
      delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
      useLibraryStore.setState({ libraryActivePath: null });
    }
  });

  it("keeps the focus outside the grid when its target appears only in a later render", () => {
    // The Library picked B elsewhere while a mounted tile A had the keys'
    // focus: the focus waits for B's tile (here B is not in the listing yet).
    // A click in a panel takes the focus away; the later render that lists B
    // must not take it back.
    const a = "/s/a.NEF", b = "/s/b.NEF", c = "/s/c.NEF";
    const before = [a, c].map(file);
    useLibraryStore.setState({ libraryActivePath: a, multiSelectedPaths: [a] });
    const { getByRole, queryByRole, rerender } = render(<StudioBrowser {...props} files={before} currentPath={a} />);
    act(() => getByRole("option", { name: "a.NEF" }).focus());
    expect(document.activeElement).toBe(getByRole("option", { name: "a.NEF" }));
    act(() => useLibraryStore.setState({ libraryActivePath: b }));
    // B is not listed: the focus waits for its tile instead of moving.
    expect(queryByRole("option", { name: "b.NEF" })).toBeNull();
    expect(document.activeElement).toBe(getByRole("option", { name: "a.NEF" }));
    const panel = document.createElement("button");
    document.body.append(panel);
    act(() => panel.focus());
    expect(document.activeElement).toBe(panel);
    rerender(<StudioBrowser {...props} files={[...before, file(b)].sort((x, y) => x.path.localeCompare(y.path))} currentPath={a} />);
    expect(getByRole("option", { name: "b.NEF" })).not.toBeNull();
    expect(document.activeElement).toBe(panel);
    panel.remove();
  });

  it("stacks the colour word below the stars in 150 px thumbnails, keeping names and targets", () => {
    // jsdom applies no stylesheet: the class says which tiles the studio.css
    // rule below applies to.
    const { container } = render(
      <StudioBrowser
        files={files}
        size={150}
        currentPath="/s/b.NEF"
        onSelect={vi.fn()}
        onOpen={vi.fn()}
        onRate={vi.fn()}
        onContextMenu={vi.fn()}
        requestThumbnails={vi.fn()}
        onClearSelection={vi.fn()}
      />,
    );
    expect(container.querySelectorAll(".studio-photo")).toHaveLength(files.length);
    expect(container.querySelectorAll(".studio-photo.is-narrow")).toHaveLength(files.length);
    // The rating controls keep their targets and names…
    const stars = Array.from(container.querySelectorAll<HTMLElement>(".studio-photo-rating button"));
    expect(stars).toHaveLength(5 * files.length);
    expect(stars[0].getAttribute("aria-label")).toBe("Rate a.NEF 1 star");
  });

  it("keeps the one row of stars and colour word above 150 px thumbnails, and in the filmstrip", () => {
    expect(grid().container.querySelectorAll(".is-narrow")).toHaveLength(0);
    expect(grid(vi.fn(), true).container.querySelectorAll(".is-narrow")).toHaveLength(0);
  });

  it("leaves the focus alone when no tile has it", () => {
    const outside = document.createElement("button");
    document.body.append(outside);
    outside.focus();
    const { rerender } = render(<StudioBrowser {...props} currentPath="/s/a.NEF" />);
    rerender(<StudioBrowser {...props} currentPath="/s/b.NEF" />);
    expect(document.activeElement).toBe(outside);
    outside.remove();
  });
});
