import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import StudioNavigation from "./StudioNavigation";
import { ContextMenuProvider } from "../../context/ContextMenuContext";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useProcessStore } from "../../store/useProcessStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import { useLibraryActions } from "../../hooks/useLibraryActions";
import { SortDirection, type AppSettings } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));
const mockInvoke = vi.mocked(invoke);

const folder = (path: string, imageCount = 0, children: object[] = [], extra = {}) => ({
  name: path.split("/").pop(),
  path,
  isDir: true,
  imageCount,
  hasSubdirs: children.length > 0,
  children,
  ...extra,
});
const tree = folder("/job", 0, [
  folder("/job/Capture", 2),
  folder("/job/Selects", 0, [folder("/job/Selects/Day 1")]),
]);

const actions = { moveHere: vi.fn(), trash: vi.fn(), addFolder: vi.fn(), removeFromLibrary: vi.fn(), paste: vi.fn() };
const settings = (extra: Partial<AppSettings> = {}) =>
  useSettingsStore.setState({ appSettings: { pinnedFolders: [], ...extra } as AppSettings });

const renderNavigation = () =>
  render(
    <ContextMenuProvider>
      <StudioNavigation onNavigate={vi.fn()} folderActions={actions} />
    </ContextMenuProvider>,
  );

beforeEach(() => {
  vi.clearAllMocks();
  // Tree commands always answer with a list.
  mockInvoke.mockImplementation(async (command) => (command === "get_pinned_folder_trees" ? [] : undefined));
  settings();
  useProcessStore.setState({ copiedFilePaths: [] });
  useLibraryStore.setState({
    sessionDirectory: "/job",
    rootPaths: ["/job"],
    currentFolderPath: "/job/Capture",
    multiSelectedPaths: ["/job/Capture/A.RAF", "/job/Capture/A.RAF?vc=abc123"],
    folderTrees: [tree] as never,
    pinnedFolderTrees: [],
    expandedFolders: new Set(["/job", "/job/Selects"]),
    isTreeLoading: false,
  });
});
afterEach(() => cleanup());

/** The folder's row (its navigate button carries the path as title). */
const row = (path: string) => screen.getAllByTitle(path)[0].parentElement!;
const openMenu = (path: string) => fireEvent.contextMenu(row(path));
/** The newest menu's item (closing menus fade out). */
const item = (name: RegExp | string) => screen.getAllByRole("menuitem", { name }).at(-1)!;
const shownFolders = () =>
  [...document.querySelectorAll(".studio-folder > button:last-child")].map((button) => button.getAttribute("title"));

describe("folder menu", () => {
  beforeEach(renderNavigation);

  it("keeps the session's own folders from being renamed or trashed", () => {
    openMenu("/job/Selects");
    expect(item(/Rename Folder/)).toHaveProperty("disabled", true);
    expect(item(/Move Folder to Trash/)).toHaveProperty("disabled", true);
  });

  it("moves the selection into another folder and trashes user folders", () => {
    openMenu("/job/Selects/Day 1");
    fireEvent.click(item("Move 2 Selected Photos Here"));
    expect(actions.moveHere).toHaveBeenCalledWith(
      ["/job/Capture/A.RAF", "/job/Capture/A.RAF?vc=abc123"],
      "/job/Selects/Day 1",
    );
    openMenu("/job/Selects/Day 1");
    fireEvent.click(item(/Move Folder to Trash/));
    expect(actions.trash).toHaveBeenCalledWith("/job/Selects/Day 1");
    openMenu("/job/Selects/Day 1");
    fireEvent.click(item(/Rename Folder/));
    expect(useUIStore.getState()).toMatchObject({
      folderActionTarget: "/job/Selects/Day 1",
      isRenameFolderModalOpen: true,
    });
  });

  it("offers no move into the folder the photos are already in", () => {
    openMenu("/job/Capture");
    expect(screen.queryByRole("menuitem", { name: /Selected Photo/ })).toBeNull();
  });

  it("pastes photos copied with ⌘C into a folder, as copies or moved", () => {
    openMenu("/job/Capture");
    expect(screen.queryByRole("menuitem", { name: /Paste/ })).toBeNull();
    // A virtual copy is the same file.
    useProcessStore.setState({ copiedFilePaths: ["/old/x.arw", "/old/x.arw?vc=1", "/old/y.arw"] });
    openMenu("/job/Capture");
    fireEvent.click(item("Paste Copies of 2 Photos Here"));
    expect(actions.paste).toHaveBeenLastCalledWith("copy", "/job/Capture");
    openMenu("/job/Capture");
    fireEvent.click(item("Move 2 Copied Photos Here"));
    expect(actions.paste).toHaveBeenLastCalledWith("move", "/job/Capture");
  });

  it("shows the folder in Finder and refreshes the folders", async () => {
    mockInvoke.mockImplementation(async (command) => (command === "get_pinned_folder_trees" ? [tree] : undefined));
    openMenu("/job/Capture");
    fireEvent.click(item(/Show in/));
    expect(mockInvoke).toHaveBeenCalledWith("show_in_finder", { path: "/job/Capture" });
    openMenu("/job/Capture");
    await act(async () => {
      fireEvent.click(item("Refresh Folders"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("get_pinned_folder_trees", expect.objectContaining({ paths: ["/job"] }));
    expect(shownFolders()).toContain("/job/Capture");
  });

  it("never offers to remove the session itself from the library", () => {
    openMenu("/job");
    expect(screen.queryByRole("menuitem", { name: "Remove from Library" })).toBeNull();
  });
});

describe("folders in the library", () => {
  it("adds folders and removes top-level folders from the list only", () => {
    useLibraryStore.setState({ sessionDirectory: null, rootPaths: ["/job"] });
    renderNavigation();
    fireEvent.click(screen.getByLabelText("Add folder"));
    expect(actions.addFolder).toHaveBeenCalledTimes(1);
    openMenu("/job/Capture");
    expect(screen.queryByRole("menuitem", { name: "Remove from Library" })).toBeNull();
    openMenu("/job");
    fireEvent.click(item("Remove from Library"));
    expect(actions.removeFromLibrary).toHaveBeenCalledWith("/job");
    expect(actions.trash).not.toHaveBeenCalled();
  });

  it("takes a removed folder off the list and the settings without touching disk", () => {
    useLibraryStore.setState({
      sessionDirectory: null,
      rootPaths: ["/job", "/photos"],
      folderTrees: [tree, folder("/photos")] as never,
      currentFolderPath: "/job/Capture",
    });
    settings({ rootFolders: ["/job", "/photos"], lastRootPath: "/job" });
    const left = vi.fn();
    const { result } = renderHook(() => useLibraryActions());
    act(() => result.current.handleRemoveRootFolder("/job", left));
    expect(useLibraryStore.getState()).toMatchObject({ rootPaths: ["/photos"], currentFolderPath: null, imageList: [] });
    expect(useLibraryStore.getState().folderTrees.map((t) => t.path)).toEqual(["/photos"]);
    expect(useSettingsStore.getState().appSettings).toMatchObject({ rootFolders: ["/photos"], lastRootPath: "/photos" });
    expect(left).toHaveBeenCalled();
    // Only the settings are saved; no file command runs.
    expect(mockInvoke.mock.calls.map(([command]) => command)).toEqual(["save_settings"]);
  });

  it("loads a folder's next level when it is expanded and collapses it again", async () => {
    useLibraryStore.setState({ folderTrees: [folder("/job", 0, [folder("/job/Capture", 2, [], { hasSubdirs: true })])] as never });
    mockInvoke.mockImplementation(async (command) =>
      command === "get_folder_children" ? [folder("/job/Capture/Day 2", 4)] : undefined,
    );
    renderNavigation();
    expect(shownFolders()).toEqual(["/job", "/job/Capture"]);
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Expand Capture"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("get_folder_children", { path: "/job/Capture", showImageCounts: false });
    expect(shownFolders()).toEqual(["/job", "/job/Capture", "/job/Capture/Day 2"]);
    expect(useLibraryStore.getState().expandedFolders.has("/job/Capture")).toBe(true);
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Collapse Capture"));
    });
    expect(shownFolders()).toEqual(["/job", "/job/Capture"]);
  });

  it("filters folder names, showing matches inside collapsed parents", () => {
    useLibraryStore.setState({ expandedFolders: new Set(["/job"]) });
    renderNavigation();
    expect(shownFolders()).not.toContain("/job/Selects/Day 1");
    const find = screen.getByLabelText("Find folders");
    // One letter does not filter yet.
    fireEvent.change(find, { target: { value: "d" } });
    expect(shownFolders()).toContain("/job/Capture");
    fireEvent.change(find, { target: { value: "day" } });
    expect(shownFolders()).toEqual(["/job", "/job/Selects", "/job/Selects/Day 1"]);
    fireEvent.change(find, { target: { value: "beach" } });
    expect(shownFolders()).toEqual([]);
    expect(screen.getByText(/No folder names contain/)).toBeTruthy();
    fireEvent.keyDown(find, { key: "Escape" });
    expect(shownFolders()).toContain("/job/Capture");
  });

  it("sorts folders by the stored sort and saves a new one", () => {
    useLibraryStore.setState({
      folderTrees: [folder("/job", 0, [folder("/job/A", 5), folder("/job/B", 9), folder("/job/C", 1)])] as never,
    });
    settings({ folderTreeSort: { key: "name", order: SortDirection.Descending } });
    renderNavigation();
    expect(shownFolders()).toEqual(["/job", "/job/C", "/job/B", "/job/A"]);
    fireEvent.click(screen.getByLabelText("Sort folders"));
    fireEvent.click(item("Photo count"));
    expect(useSettingsStore.getState().appSettings?.folderTreeSort).toEqual({
      key: "imageCount",
      order: SortDirection.Descending,
    });
    expect(shownFolders()).toEqual(["/job", "/job/B", "/job/A", "/job/C"]);
  });

  it("shows photo counts in sessions and elsewhere only when asked for", () => {
    renderNavigation();
    expect(row("/job/Capture").textContent).toContain("2");
    cleanup();
    useLibraryStore.setState({ sessionDirectory: null });
    renderNavigation();
    expect(row("/job/Capture").textContent).not.toContain("2");
    cleanup();
    settings({ enableFolderImageCounts: true });
    renderNavigation();
    expect(row("/job/Capture").textContent).toContain("2");
  });
});

describe("favourite folders", () => {
  it("lists favourites on top, from the settings key the folder tree used", async () => {
    settings({ pinnedFolders: ["/photos/Trip"] });
    useLibraryStore.setState({ pinnedFolderTrees: [folder("/photos/Trip", 3)] as never });
    renderNavigation();
    expect(screen.getByText("Favorites")).toBeTruthy();
    expect(shownFolders()[0]).toBe("/photos/Trip");
    openMenu("/photos/Trip");
    await act(async () => {
      fireEvent.click(item("Remove from Favorites"));
    });
    expect(useSettingsStore.getState().appSettings?.pinnedFolders).toEqual([]);
    expect(screen.queryByText("Favorites")).toBeNull();
  });

  it("adds a folder to the favourites and loads its tree", async () => {
    mockInvoke.mockImplementation(async (command) =>
      command === "get_pinned_folder_trees" ? [folder("/job/Selects")] : undefined,
    );
    renderNavigation();
    openMenu("/job/Selects");
    await act(async () => {
      fireEvent.click(item("Add to Favorites"));
    });
    expect(useSettingsStore.getState().appSettings?.pinnedFolders).toEqual(["/job/Selects"]);
    expect(useLibraryStore.getState().pinnedFolderTrees.map((t) => t.path)).toEqual(["/job/Selects"]);
    expect(screen.getByText("Favorites")).toBeTruthy();
  });
});

describe("offline photographs in the Folders tool", () => {
  it("marks folders holding offline photographs and lists folders gone from disk at their place", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_pinned_folder_trees") return [];
      if (command === "session_offline")
        return [
          { path: "/job/Capture/B.RAF", volume: null, mtime: 1, folder_missing: false },
          { path: "/job/Day 2/C.RAF", volume: null, mtime: 1, folder_missing: true },
          { path: "/job/Day 2/D.RAF", volume: null, mtime: 1, folder_missing: true },
          { path: "/elsewhere/E.RAF", volume: null, mtime: 1, folder_missing: true },
        ];
      if (command === "catalog_folders") return { catalog: false, volumes: [] };
      return undefined;
    });
    const navigate = vi.fn();
    const locate = vi.fn();
    render(
      <ContextMenuProvider>
        <StudioNavigation onNavigate={navigate} folderActions={{ ...actions, locateFolder: locate }} />
      </ContextMenuProvider>,
    );
    await act(async () => {});
    // Capture holds one; the session root three below it (not the one elsewhere).
    expect(screen.getAllByText("?1")).toHaveLength(2); // Capture, and the folder outside the session
    expect(screen.getByText("?3")).toBeTruthy();
    // The renamed folder stays in the session's tree, marked, and opens its listing.
    const missing = screen.getByText("Day 2").closest("button")!;
    expect(missing.textContent).toContain("?2");
    expect(missing.getAttribute("title")).toBe(
      "/job/Day 2 is missing (renamed, moved or deleted). Its photographs keep their edits; right-click one › Locate Folder…",
    );
    expect(missing.closest(".studio-folder")!.className).toContain("is-offline");
    fireEvent.click(missing);
    expect(navigate).toHaveBeenCalledWith("/job/Day 2");
    // Its menu offers to locate it, nothing that would touch disk.
    fireEvent.contextMenu(missing);
    expect(screen.queryByText("Rename Folder…")).toBeNull();
    fireEvent.click(screen.getByText("Locate Folder…"));
    expect(locate).toHaveBeenCalledWith("/job/Day 2");
    // A folder outside every listed one is listed under Missing.
    expect(screen.getByText("Missing")).toBeTruthy();
    expect(screen.getByText("elsewhere")).toBeTruthy();
  });
});

describe("a catalog's folders by drive", () => {
  const view = {
    catalog: true,
    volumes: [
      {
        key: "id:CARD",
        id: "CARD",
        name: "Card",
        mount_point: "/Volumes/Card",
        online: false,
        free_bytes: null,
        total_bytes: null,
        folders: [{ path: "/Volumes/Card/2026", photos: 3, offline: 3, exists: false }],
        roots: [],
      },
      {
        key: "id:SYS",
        id: "SYS",
        name: "Macintosh HD",
        mount_point: "/",
        online: true,
        free_bytes: 200e9,
        total_bytes: 1000e9,
        folders: [{ path: "/Users/me/Relinked", photos: 1, offline: 0, exists: true }],
        roots: ["/job"],
      },
    ],
  };

  it("groups folders under their drive with its state and free space", async () => {
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_pinned_folder_trees") {
        const paths = (args as { paths: string[] }).paths;
        return paths.includes("/Users/me/Relinked") ? [folder("/Users/me/Relinked", 1)] : [];
      }
      if (command === "session_offline")
        return [{ path: "/Volumes/Card/2026/A.NEF", volume: "Card", mtime: 1, folder_missing: true }];
      if (command === "catalog_folders") return view;
      return undefined;
    });
    const navigate = vi.fn();
    render(
      <ContextMenuProvider>
        <StudioNavigation onNavigate={navigate} folderActions={actions} />
      </ContextMenuProvider>,
    );
    await act(async () => {});
    await act(async () => {});
    expect(mockInvoke).toHaveBeenCalledWith("catalog_folders", { directory: "/job", roots: ["/job"] });
    // The disconnected drive and its folder, known from the catalog.
    const card = document.querySelector('[data-volume="id:CARD"]')!;
    expect(card.getAttribute("data-online")).toBe("false");
    expect(card.textContent).toContain("Offline");
    const offlineFolder = screen.getByText("2026").closest("button")!;
    expect(offlineFolder.getAttribute("title")).toBe("/Volumes/Card/2026 is on “Card”, which is not connected.");
    expect(offlineFolder.textContent).toContain("?1");
    fireEvent.click(offlineFolder);
    expect(navigate).toHaveBeenCalledWith("/Volumes/Card/2026");
    // The connected drive: free space, the library folder and the folder a
    // photograph was relinked to.
    const system = document.querySelector('[data-volume="id:SYS"]')!;
    expect(system.textContent).toContain("200 GB / 1 TB");
    expect(screen.getByText("Relinked")).toBeTruthy();
    expect(screen.getByText("job")).toBeTruthy();
    // A drive folds away.
    fireEvent.click(screen.getByLabelText("Collapse Card"));
    expect(screen.queryByText("2026")).toBeNull();
    fireEvent.click(screen.getByLabelText("Expand Card"));
    expect(screen.getByText("2026")).toBeTruthy();
  });
});
