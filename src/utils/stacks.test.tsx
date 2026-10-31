import "../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { RawStatus, SortDirection, type ImageFile } from "../components/ui/AppProperties";
import StudioBrowser from "../components/studio/StudioBrowser";
import { computeGroupedLibrary, type LibraryQueryState } from "../hooks/useSortedLibrary";
import { useLibraryStore } from "../store/useLibraryStore";
import { arrangeStacks, indexStacks, setStacks, useStackStore, type WebStack } from "../store/useStackStore";
import { useUIStore } from "../store/useUIStore";
import { canUndo, getUndoLabel, redo, undo, useCatalogUndoStore } from "./appUndo";
import { runAppCommand } from "./appCommands";
import { autoStackScope, groupIntoStack, selectShownMembers, toggleStacks, useStackCommands } from "./stacks";
import { buildComboMap } from "./keyboardUtils";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []), convertFileSrc: (p: string) => p }));
vi.mock("../activity/notify", () => ({
  notify: { error: vi.fn(), info: vi.fn(), success: vi.fn(), warning: vi.fn(), dismiss: vi.fn(), update: vi.fn(), isActive: vi.fn(() => false) },
}));

const file = (path: string, rating = 0): ImageFile => ({
  path, rating, tags: null, modified: 0, is_edited: false, is_raw: true,
  exif: null, is_virtual_copy: false, is_cloud_placeholder: false, group_id: null,
});
const names = (list: ImageFile[]) => list.map((f) => f.path.split("/").pop());
const files = ["a", "b", "c", "d", "e"].map((n) => file(`/job/Capture/${n}.ARW`));
const stack = (id: string, members: string[], collapsed = true): WebStack => ({
  id, collapsed, members: members.map((n) => `/job/Capture/${n}.ARW`),
});

const query = (over: Partial<LibraryQueryState> = {}): LibraryQueryState => ({
  imageList: files, imageRatings: {}, imageFlags: {},
  filterCriteria: { flag: "all", rating: 0, colors: [], rawStatus: RawStatus.All },
  searchCriteria: { text: "", tags: [], mode: "OR" },
  sortCriteria: { key: "name", order: SortDirection.Ascending },
  stackView: true,
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  setStacks(null, []);
  useLibraryStore.setState({
    sessionDirectory: "/job", imageList: files, activeAlbumId: null,
    multiSelectedPaths: [], libraryActivePath: null, imageRatings: {}, imageFlags: {},
  });
  useUIStore.setState({ activeView: "library" });
  useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState());
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("the grid's stacks", () => {
  it("shows a collapsed stack as its top with the count and an expanded one together", () => {
    // Top d, then b: the stack sits where b sorts (first of its members).
    const byPath = indexStacks([stack("s", ["d", "b"])]);
    const collapsed = arrangeStacks(files, byPath);
    expect(names(collapsed.list)).toEqual(["a.ARW", "d.ARW", "c.ARW", "e.ARW"]);
    expect(collapsed.marks.get(files[3].path)).toMatchObject({ count: 2, collapsed: true, top: true, position: "only" });
    const expanded = arrangeStacks(files, indexStacks([stack("s", ["d", "b", "e"], false)]));
    expect(names(expanded.list)).toEqual(["a.ARW", "d.ARW", "b.ARW", "e.ARW", "c.ARW"]);
    expect([...expanded.marks.values()].map((m) => m.position)).toEqual(["first", "middle", "last"]);
  });

  it("promotes the next member when the top leaves the listing (Move to Trash)", () => {
    const byPath = indexStacks([stack("s", ["a", "b", "c"])]);
    const listing = files.filter((f) => !f.path.endsWith("a.ARW"));
    const { list, marks } = arrangeStacks(listing, byPath);
    expect(names(list)).toEqual(["b.ARW", "d.ARW", "e.ARW"]);
    expect(marks.get(files[1].path)?.count).toBe(2);
  });

  it("collapses in folder views only; filters, searches and albums list members singly", () => {
    const stacks = indexStacks([stack("s", ["a", "b", "c"])]);
    expect(names(computeGroupedLibrary(query({ stacks }), {}).displayList)).toEqual(["a.ARW", "d.ARW", "e.ARW"]);
    // An album or smart album.
    expect(computeGroupedLibrary(query({ stacks, stackView: false }), {}).displayList).toHaveLength(5);
    // A filter hiding the top shows the matching member inside the stack.
    const rated = files.map((f) => (f.path.endsWith("b.ARW") ? { ...f, rating: 4 } : f));
    const filtered = computeGroupedLibrary(
      query({ stacks, imageList: rated, filterCriteria: { flag: "all", rating: 3, colors: [], rawStatus: RawStatus.All } }),
      {},
    );
    expect(names(filtered.displayList)).toEqual(["b.ARW"]);
    expect(filtered.stackMarks).toBeUndefined();
  });

  it("badges the top with its count; a click toggles the stack", async () => {
    setStacks("/job", [stack("s", ["a", "b"])]);
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "stacks_set_collapsed" ? { library: "/job", change: { before: [1], after: [2] } } : [],
    );
    const { list, marks } = arrangeStacks(files, useStackStore.getState().byPath);
    render(
      <StudioBrowser files={list} stackMarks={marks} onSelect={vi.fn()} onOpen={vi.fn()} onRate={vi.fn()}
        onContextMenu={vi.fn()} requestThumbnails={vi.fn()} />,
    );
    expect(screen.getByRole("button", { name: "Stack of 2 photos – expand" })).toBeTruthy();
    // Expanded, a member says where it is.
    cleanup();
    const open = arrangeStacks(files, indexStacks([stack("t", ["c", "d", "e"], false)]));
    render(
      <StudioBrowser files={open.list} stackMarks={open.marks} onSelect={vi.fn()} onOpen={vi.fn()} onRate={vi.fn()}
        onContextMenu={vi.fn()} requestThumbnails={vi.fn()} />,
    );
    expect(screen.getByRole("img", { name: "2 of 3 in the stack" })).toBeTruthy();
    cleanup();
    render(
      <StudioBrowser files={list} stackMarks={marks} onSelect={vi.fn()} onOpen={vi.fn()} onRate={vi.fn()}
        onContextMenu={vi.fn()} requestThumbnails={vi.fn()} />,
    );
    const badge = screen.getByRole("button", { name: "Stack of 2 photos – expand" });
    expect(badge.textContent).toBe("2");
    fireEvent.click(badge);
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("stacks_set_collapsed", { paths: [files[0].path], collapsed: null }),
    );
    expect(getUndoLabel()).toBe("Undo Stacking");
  });
});

describe("stack commands", () => {
  it("groups the selection with the active photo on top as one undo step", async () => {
    const change = { before: [], after: [{ id: "s" }] };
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "stacks_group") return { library: "/job", change };
      if (command === "stacks_list") return [stack("s", ["c", "a"])];
      return null;
    });
    useLibraryStore.setState({ multiSelectedPaths: [files[0].path, files[2].path], libraryActivePath: files[2].path });
    await groupIntoStack();
    expect(invoke).toHaveBeenCalledWith("stacks_group", { paths: [files[2].path, files[0].path] });
    // The new stack is collapsed: the hidden member leaves the selection.
    expect(useLibraryStore.getState().multiSelectedPaths).toEqual([files[2].path]);
    expect(canUndo()).toBe(true);
    await undo();
    expect(invoke).toHaveBeenCalledWith("stacks_restore", { library: "/job", change, undo: true });
    await redo();
    expect(invoke).toHaveBeenCalledWith("stacks_restore", { library: "/job", change, undo: false });
  });

  it("moves a selected member to its stack's top before the collapse hides it", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "stacks_set_collapsed") return { library: "/job", change: { before: [1], after: [2] } };
      if (command === "stacks_list") return [stack("s", ["a", "b", "c"], true)];
      return null;
    });
    setStacks("/job", [stack("s", ["a", "b", "c"], false)]);
    useLibraryStore.setState({ multiSelectedPaths: [files[2].path], libraryActivePath: files[2].path });
    // The grid drops selected photos it no longer shows when the stacks
    // change: the selection must already be the top by then.
    const seen: string[][] = [];
    const stop = useStackStore.subscribe(() => seen.push(useLibraryStore.getState().multiSelectedPaths));
    await toggleStacks();
    stop();
    expect(seen).toEqual([[files[0].path]]);
    expect(useLibraryStore.getState().libraryActivePath).toBe(files[0].path);
  });

  it("binds ⇧S to Move to Top in the Library and keeps it for soft proofing in Develop", () => {
    expect(buildComboMap(null).get("shift+KeyS")?.sort()).toEqual(["stack_move_top", "toggle_soft_proof"]);
  });

  it("auto-stacks the selection when it holds several photos, else the folder", () => {
    useLibraryStore.setState({ multiSelectedPaths: [files[0].path] });
    expect(autoStackScope()).toHaveLength(5);
    useLibraryStore.setState({ multiSelectedPaths: [files[0].path, files[1].path] });
    expect(autoStackScope()).toEqual([files[0].path, files[1].path]);
  });

  it("makes no undo step when nothing changed", async () => {
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "stacks_set_collapsed" ? { library: "/job", change: { before: [], after: [] } } : [],
    );
    await toggleStacks([files[0].path]);
    expect(canUndo()).toBe(false);
  });

  it("keeps a selection of a collapsed stack to its top (ratings act on the top only)", () => {
    setStacks("/job", [stack("s", ["b", "a"])]);
    useLibraryStore.setState({ multiSelectedPaths: [files[0].path, files[1].path, files[3].path], libraryActivePath: files[0].path });
    selectShownMembers();
    expect(useLibraryStore.getState().multiSelectedPaths).toEqual([files[1].path, files[3].path]);
    expect(useLibraryStore.getState().libraryActivePath).toBe(files[1].path);
  });

  it("runs ⌘G, ⇧⌘G and S through the registered commands in the Library only", async () => {
    vi.mocked(invoke).mockImplementation(async (command) =>
      String(command).startsWith("stacks_") && command !== "stacks_list"
        ? { library: "/job", change: { before: [], after: [] } }
        : [],
    );
    const hook = renderHook(() => useStackCommands());
    setStacks("/job", [stack("s", ["a", "b"])]);
    useLibraryStore.setState({ multiSelectedPaths: [files[0].path], libraryActivePath: files[0].path });
    expect(runAppCommand("stack_group")).toBe(false); // one photo
    expect(runAppCommand("stack_toggle")).toBe(true);
    expect(runAppCommand("stack_unstack")).toBe(true);
    useLibraryStore.setState({ multiSelectedPaths: [files[2].path, files[3].path] });
    expect(runAppCommand("stack_group")).toBe(true);
    useUIStore.setState({ activeView: "editor" });
    expect(runAppCommand("stack_toggle")).toBe(false); // S is the crop tool there
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("stacks_group", expect.anything()));
    expect(invoke).toHaveBeenCalledWith("stacks_set_collapsed", { paths: [files[0].path], collapsed: null });
    expect(invoke).toHaveBeenCalledWith("stacks_unstack", { paths: [files[0].path] });
    hook.unmount();
  });
});
