import "../../i18n";
import { toast } from "react-toastify";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import StudioAlbums, { dropPlacement, type SessionAlbum } from "./StudioAlbums";
import { flattenAlbumTree, withDeleted, withMoved, withNewItem } from "./libraryAlbums";
import { useLibraryStore } from "../../store/useLibraryStore";
import type { AlbumItem } from "../ui/AppProperties";
import { ContextMenuProvider } from "../../context/ContextMenuContext";
import { useSmartAlbumEditor } from "./SmartAlbumEditor";
import { CATALOG_CHANGED, catalogChanged } from "../../utils/catalogWrites";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn() } }));
const mockInvoke = vi.mocked(invoke);
let albums: SessionAlbum[] = [{ id: "p", name: "Picks", parent: null, count: 2 }];

beforeEach(() => {
  albums = [{ id: "p", name: "Picks", parent: null, count: 2 }];
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command, args) => {
    const a = args as Record<string, unknown>;
    if (command === "session_albums") return albums;
    if (command === "session_album_paths") return ["/job/Capture/a.arw", "/job/Capture/b.arw"];
    if (command === "session_create_album") {
      albums = [...albums, { id: "n", name: a.name as string, parent: null, count: (a.paths as string[]).length }];
      return albums[albums.length - 1];
    }
    if (command === "session_delete_album") albums = albums.filter((album) => album.id !== a.id);
    return undefined;
  });
  useLibraryStore.setState({
    sessionDirectory: "/job",
    activeAlbumId: null,
    multiSelectedPaths: ["/job/Capture/c.arw"],
  });
});
afterEach(cleanup);

const renderTool = async (select = vi.fn()) => {
  render(
    <ContextMenuProvider>
      <StudioAlbums onSelectAlbum={select} />
    </ContextMenuProvider>,
  );
  await act(async () => {});
  return select;
};

describe("Albums tool", () => {
  it("opens an album in the grid", async () => {
    const select = await renderTool();
    await act(async () => {
      fireEvent.click(screen.getByText("Picks"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("session_album_paths", { directory: "/job", id: "p" });
    expect(select).toHaveBeenCalledWith(
      "session-album:p",
      "Picks",
      ["/job/Capture/a.arw", "/job/Capture/b.arw"],
      false,
    );
  });

  it("makes an album from the selection", async () => {
    await renderTool();
    fireEvent.click(screen.getByLabelText("New album"));
    fireEvent.click(await screen.findByText("New album from 1 selected photograph"));
    const input = screen.getByLabelText("New album name");
    fireEvent.change(input, { target: { value: "Web" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter", currentTarget: input });
    });
    expect(mockInvoke).toHaveBeenCalledWith("session_create_album", {
      directory: "/job",
      name: "Web",
      paths: ["/job/Capture/c.arw"],
      kind: "album",
      parent: null,
    });
    expect(screen.getByText("Web")).toBeTruthy();
  });

  it("adds the selection and refreshes the album shown in the grid", async () => {
    useLibraryStore.setState({ activeAlbumId: "session-album:p" });
    const select = await renderTool();
    fireEvent.contextMenu(screen.getByText("Picks"));
    await act(async () => {
      fireEvent.click(await screen.findByText("Add 1 selected photograph"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("session_album_add", {
      directory: "/job",
      id: "p",
      paths: ["/job/Capture/c.arw"],
    });
    expect(select).toHaveBeenCalledWith("session-album:p", "Picks", expect.any(Array), true);
  });

  it("deletes an album only after confirmation", async () => {
    await renderTool();
    fireEvent.contextMenu(screen.getByText("Picks"));
    fireEvent.click(await screen.findByText("Delete album…"));
    expect(screen.getByRole("alertdialog").textContent).toContain("Its photographs stay");
    await act(async () => {
      fireEvent.click(screen.getByText("Delete"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("session_delete_album", { directory: "/job", id: "p" });
    expect(screen.queryByText("Picks")).toBeNull();
  });

});

describe("Missing Photographs", () => {
  it("is listed only while photographs are offline and opens them in the grid", async () => {
    await renderTool();
    expect(screen.queryByText("Missing Photographs")).toBeNull();
    cleanup();

    const offline = [
      { path: "/Volumes/Card/A.NEF", volume: "Card", mtime: 1 },
      { path: "/job/Capture/b.arw", volume: null, mtime: 2 },
    ];
    const base = mockInvoke.getMockImplementation()!;
    mockInvoke.mockImplementation(async (command, args) =>
      command === "session_offline" ? offline : base(command, args),
    );
    const select = await renderTool();
    const row = screen.getByText("Missing Photographs").closest("button")!;
    expect(row.textContent).toContain("2");
    expect(row.getAttribute("title")).toBe("Photographs whose file is missing or on a disconnected drive. They keep their edits; locate them to relink.");
    await act(async () => {
      fireEvent.click(row);
    });
    expect(select).toHaveBeenCalledWith(
      "session-missing",
      "Missing Photographs",
      ["/Volumes/Card/A.NEF", "/job/Capture/b.arw"],
      false,
    );

    // After a relink the count follows.
    offline.pop();
    await act(async () => {
      window.dispatchEvent(new Event("rawtonic:offline-changed"));
    });
    expect(screen.getByText("Missing Photographs").closest("button")!.textContent).toContain("1");
  });
});

describe("Previous Import", () => {
  it("lists the newest import batch while it holds photographs and follows new imports", async () => {
    await renderTool();
    expect(screen.queryByText("Previous Import")).toBeNull();
    cleanup();

    let paths = ["/job/Capture/n1.arw", "/job/Capture/n2.arw"];
    const base = mockInvoke.getMockImplementation()!;
    mockInvoke.mockImplementation(async (command, args) =>
      command === "session_previous_import"
        ? { batch: { id: 3, kind: "sync", created_at: "2026-10-13 10:00:00", count: paths.length }, paths }
        : base(command, args),
    );
    const select = await renderTool();
    const row = screen.getByText("Previous Import").closest("button")!;
    expect(row.textContent).toContain("2");
    await act(async () => {
      fireEvent.click(row);
    });
    expect(select).toHaveBeenCalledWith("session-previous-import", "Previous Import", paths, false);
    // A new ingest: the count follows.
    paths = ["/job/Capture/n3.arw"];
    await act(async () => {
      window.dispatchEvent(new Event("rawtonic:imports-changed"));
    });
    expect(screen.getByText("Previous Import").closest("button")!.textContent).toContain("1");
  });
});

describe("album groups", () => {
  const tree: SessionAlbum[] = [
    { id: "g", name: "Client", parent: null, count: 0, kind: "group" },
    { id: "a", name: "Selects", parent: "g", count: 3, kind: "album" },
    { id: "inner", name: "Days", parent: "g", count: 0, kind: "group" },
    { id: "b", name: "Web", parent: null, count: 1, kind: "album" },
  ];

  it("computes where a dragged album lands", () => {
    expect(dropPlacement(tree, "b", "a", "before")).toEqual({ parent: "g", index: 0 });
    expect(dropPlacement(tree, "b", "a", "after")).toEqual({ parent: "g", index: 1 });
    expect(dropPlacement(tree, "b", "inner", "into")).toEqual({ parent: "inner", index: 0 });
    expect(dropPlacement(tree, "a", "g", "before")).toEqual({ parent: null, index: 0 });
    expect(dropPlacement(tree, "a", null, "after")).toEqual({ parent: null, index: 2 });
    // Albums hold no albums; a group never goes into itself or its own groups.
    expect(dropPlacement(tree, "a", "b", "into")).toBeNull();
    expect(dropPlacement(tree, "g", "inner", "into")).toBeNull();
    expect(dropPlacement(tree, "g", "a", "after")).toBeNull();
    expect(dropPlacement(tree, "g", "g", "before")).toBeNull();
  });

  it("shows groups as collapsible folders and makes groups and albums inside them", async () => {
    albums = tree;
    await renderTool();
    expect(screen.getByText("Selects")).toBeTruthy();
    fireEvent.click(screen.getByText("Client"));
    expect(screen.queryByText("Selects")).toBeNull();
    fireEvent.click(screen.getByText("Client"));
    fireEvent.contextMenu(screen.getByText("Client"));
    fireEvent.click(await screen.findByText("New group in this group"));
    const input = screen.getByLabelText("New group name");
    fireEvent.change(input, { target: { value: "Week 2" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    expect(mockInvoke).toHaveBeenCalledWith("session_create_album", {
      directory: "/job",
      name: "Week 2",
      paths: [],
      kind: "group",
      parent: "g",
    });
    // Photographs can only be dropped on albums.
    expect(screen.getByText("Client").closest("button")?.dataset.albumDrop).toBeUndefined();
    expect(screen.getByText("Selects").closest("button")?.dataset.albumDrop).toBe("a");
  });
});

describe("app-wide albums outside sessions", () => {
  // albums.json as the backend keeps it (get_albums / save_albums).
  let stored: AlbumItem[];
  const saved = () =>
    mockInvoke.mock.calls.filter(([command]) => command === "save_albums").map(([, args]) => (args as { tree: AlbumItem[] }).tree);
  beforeEach(() => {
    stored = [
      {
        type: "group",
        id: "g",
        name: "Travel",
        icon: "plane",
        children: [{ type: "album", id: "a", name: "Lisbon", icon: "sun", images: ["/p/1.arw", "/p/2.arw"] }],
      },
      { type: "album", id: "b", name: "Family", images: ["/p/3.arw"] },
    ];
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_albums") return structuredClone(stored);
      if (command === "save_albums") stored = structuredClone((args as { tree: AlbumItem[] }).tree);
      return undefined;
    });
    useLibraryStore.setState({
      sessionDirectory: null,
      activeAlbumId: null,
      albumTree: [],
      multiSelectedPaths: ["/p/4.arw"],
    });
  });

  it("lists the stored albums and groups and opens an album with its photographs", async () => {
    const select = await renderTool();
    expect(screen.getByText("Travel")).toBeTruthy();
    expect(screen.getByText("Lisbon").closest("button")?.textContent).toContain("2");
    await act(async () => {
      fireEvent.click(screen.getByText("Lisbon"));
    });
    expect(select).toHaveBeenCalledWith("a", "Lisbon", ["/p/1.arw", "/p/2.arw"], false);
    expect(mockInvoke.mock.calls.some(([command]) => String(command).startsWith("session_"))).toBe(false);
  });

  it("makes an album from the selection and keeps every other album as it was", async () => {
    await renderTool();
    fireEvent.click(screen.getByLabelText("New album"));
    fireEvent.click(await screen.findByText("New album from 1 selected photograph"));
    const input = screen.getByLabelText("New album name");
    fireEvent.change(input, { target: { value: "Web" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    const [tree] = saved();
    expect(tree.slice(0, 2)).toEqual([
      {
        type: "group",
        id: "g",
        name: "Travel",
        icon: "plane",
        children: [{ type: "album", id: "a", name: "Lisbon", icon: "sun", images: ["/p/1.arw", "/p/2.arw"] }],
      },
      { type: "album", id: "b", name: "Family", images: ["/p/3.arw"] },
    ]);
    expect(tree[2]).toMatchObject({ type: "album", name: "Web", images: ["/p/4.arw"] });
    expect(screen.getByText("Web")).toBeTruthy();
  });

  it("reads the stored albums before each change, so a stale list never overwrites them", async () => {
    await renderTool();
    // Another window (or the photo menu) added an album meanwhile.
    stored = [...stored, { type: "album", id: "c", name: "Prints", images: [] }];
    fireEvent.contextMenu(screen.getByText("Family"));
    await act(async () => {
      fireEvent.click(await screen.findByText("Add 1 selected photograph"));
    });
    const [tree] = saved();
    expect(tree.map((item) => item.name)).toEqual(["Travel", "Family", "Prints"]);
    expect(tree[1]).toEqual({ type: "album", id: "b", name: "Family", images: ["/p/3.arw", "/p/4.arw"] });
  });

  it("saves nothing when the stored albums cannot be read", async () => {
    await renderTool();
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_albums") throw new Error("albums.json: unexpected end");
      return undefined;
    });
    fireEvent.contextMenu(screen.getByText("Family"));
    await act(async () => {
      fireEvent.click(await screen.findByText("Add 1 selected photograph"));
    });
    expect(saved()).toEqual([]);
  });

  it("removes the selection from the album shown and refreshes the grid", async () => {
    useLibraryStore.setState({ activeAlbumId: "a", multiSelectedPaths: ["/p/2.arw"] });
    const select = await renderTool();
    fireEvent.contextMenu(screen.getByText("Lisbon"));
    await act(async () => {
      fireEvent.click(await screen.findByText("Remove selected photographs"));
    });
    expect(stored[0]).toMatchObject({ children: [{ id: "a", images: ["/p/1.arw"] }] });
    expect(select).toHaveBeenCalledWith("a", "Lisbon", ["/p/1.arw"], true);
  });

  it("renames albums and deletes a group with its albums after confirmation", async () => {
    await renderTool();
    fireEvent.contextMenu(screen.getByText("Family"));
    fireEvent.click(await screen.findByText("Rename"));
    const input = screen.getByLabelText("Rename Family");
    fireEvent.change(input, { target: { value: "Kids" } });
    await act(async () => {
      fireEvent.keyDown(input, { key: "Enter" });
    });
    expect(stored[1]).toEqual({ type: "album", id: "b", name: "Kids", images: ["/p/3.arw"] });
    fireEvent.contextMenu(screen.getByText("Travel"));
    fireEvent.click(await screen.findByText("Delete group…"));
    expect(screen.getByRole("alertdialog").textContent).toContain("Their photographs stay");
    await act(async () => {
      fireEvent.click(screen.getByText("Delete"));
    });
    expect(stored).toEqual([{ type: "album", id: "b", name: "Kids", images: ["/p/3.arw"] }]);
    expect(screen.queryByText("Lisbon")).toBeNull();
  });

  it("flattens the tree and moves items between groups, never into themselves", () => {
    expect(flattenAlbumTree(stored).map(({ id, parent, kind, count }) => [id, parent, kind, count])).toEqual([
      ["g", null, "group", 0],
      ["a", "g", "album", 2],
      ["b", null, "album", 1],
    ]);
    const moved = withMoved(stored, "b", "g", 1)!;
    expect((moved[0] as { children: AlbumItem[] }).children.map((item) => item.id)).toEqual(["a", "b"]);
    expect(moved).toHaveLength(1);
    expect(withMoved(stored, "g", "g", 0)).toBeNull();
    const nested = withNewItem(stored, { kind: "group", name: "Days", parent: "g", paths: ["/x"], id: "d" })!;
    expect(withMoved(nested, "g", "d", 0)).toBeNull();
    expect(withNewItem(stored, { kind: "album", name: "X", parent: "a", paths: [], id: "x" })).toBeNull();
    expect(withDeleted(stored, "missing")).toBeNull();
    // The input is never changed.
    expect(stored).toHaveLength(2);
  });
});

describe("switching sessions", () => {
  it("drops album lists and counts the closed session answers late", async () => {
    const late: Array<() => void> = [];
    const base = mockInvoke.getMockImplementation()!;
    mockInvoke.mockImplementation(async (command, args) => {
      const directory = (args as { directory?: string })?.directory;
      if (directory === "/old" && command === "session_albums")
        return new Promise((resolve) => late.push(() => resolve([{ id: "o", name: "Old album", parent: null, count: 1 }])));
      if (directory === "/old" && command === "session_previous_import")
        return new Promise((resolve) =>
          late.push(() => resolve({ batch: { id: 1, kind: "ingest", created_at: "", count: 5 }, paths: ["1", "2", "3", "4", "5"] })),
        );
      if (directory === "/old" && command === "session_offline")
        return new Promise((resolve) => late.push(() => resolve([{ path: "/old/a.arw", reason: "missing" }])));
      if (command === "session_previous_import") return { batch: null, paths: [] };
      if (command === "session_offline") return [];
      return base(command, args);
    });
    useLibraryStore.setState({ sessionDirectory: "/old" });
    await renderTool();
    await act(async () => useLibraryStore.setState({ sessionDirectory: "/job" }));
    expect(screen.getByText("Picks")).toBeTruthy();
    await act(async () => late.forEach((answer) => answer()));
    expect(screen.queryByText("Old album")).toBeNull();
    expect(screen.queryByText("Previous Import")).toBeNull();
    expect(screen.queryByText("Missing Photographs")).toBeNull();
    expect(screen.getByText("Picks")).toBeTruthy();
  });
});

describe("Smart albums in the Albums tool", () => {
  const smart: SessionAlbum = {
    id: "s",
    name: "Five stars",
    parent: null,
    count: 1,
    kind: "smart",
    rules: { field: "rating", op: "is", value: 5 },
  };

  it("lists a smart album with its count, opens it, refuses drops and edits its rules", async () => {
    albums = [...albums, smart];
    const select = await renderTool();
    const row = screen.getByText("Five stars").closest("button")!;
    expect(row.getAttribute("data-album-drop")).toBeNull();
    expect(row.getAttribute("data-drop-refused")).toContain("rules");
    await act(async () => {
      fireEvent.click(row);
    });
    expect(select).toHaveBeenCalledWith("session-album:s", "Five stars", expect.any(Array), false);
    fireEvent.contextMenu(row);
    expect(screen.queryByText("Add selected photographs")).toBeNull();
    fireEvent.click(await screen.findByText("Edit smart album…"));
    expect(useSmartAlbumEditor.getState().draft).toEqual({
      id: "s",
      name: "Five stars",
      parent: null,
      rules: { combine: "all", rules: [{ field: "rating", op: "is", value: 5 }] },
    });
    act(() => useSmartAlbumEditor.getState().close());
  });

  it("refreshes the shown smart album although the parent rendered meanwhile", async () => {
    vi.useFakeTimers();
    try {
      albums = [...albums, smart];
      useLibraryStore.setState({ activeAlbumId: "session-album:s" });
      const first = vi.fn();
      const view = render(
        <ContextMenuProvider>
          <StudioAlbums onSelectAlbum={first} />
        </ContextMenuProvider>,
      );
      await act(async () => {});
      act(() => catalogChanged());
      // The parent renders with a new callback before the refresh runs.
      const second = vi.fn();
      view.rerender(
        <ContextMenuProvider>
          <StudioAlbums onSelectAlbum={second} />
        </ContextMenuProvider>,
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(400);
      });
      expect(second).toHaveBeenCalledWith("session-album:s", "Five stars", expect.any(Array), true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("counts smart albums again when the grid gets a new list (a rename)", async () => {
    vi.useFakeTimers();
    try {
      albums = [...albums, smart];
      await renderTool();
      albums = albums.map((a) => (a.id === "s" ? { ...a, count: 0 } : a));
      act(() => useLibraryStore.setState({ imageList: [] }));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(400);
      });
      expect(screen.getByText("Five stars").closest("button")!.textContent).toContain("0");
    } finally {
      vi.useRealTimers();
    }
  });

  it("drops a refresh that answers after the grid moved on", async () => {
    vi.useFakeTimers();
    try {
      albums = [...albums, smart];
      useLibraryStore.setState({ activeAlbumId: "session-album:s" });
      let answer: (paths: string[]) => void = () => {};
      const select = await renderTool();
      mockInvoke.mockImplementation(async (command) => {
        if (command === "session_albums") return albums;
        if (command === "session_album_paths") return new Promise((resolve) => (answer = resolve));
        return undefined;
      });
      act(() => catalogChanged());
      await act(async () => {
        await vi.advanceTimersByTimeAsync(400);
      });
      // The user opens a folder before the smart album's paths arrive.
      useLibraryStore.setState({ activeAlbumId: null });
      await act(async () => answer(["/job/Capture/a.arw"]));
      expect(select).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("tells searches and smart albums when album membership changes", async () => {
    const changed = vi.fn();
    window.addEventListener(CATALOG_CHANGED, changed);
    await renderTool();
    fireEvent.contextMenu(screen.getByText("Picks"));
    await act(async () => {
      fireEvent.click(await screen.findByText("Add 1 selected photograph"));
    });
    expect(changed).toHaveBeenCalled();
    window.removeEventListener(CATALOG_CHANGED, changed);
  });

  it("offers a new smart album in a session, in a group too", async () => {
    await renderTool();
    fireEvent.click(screen.getByLabelText("New album"));
    fireEvent.click(await screen.findByText("New smart album…"));
    expect(useSmartAlbumEditor.getState().draft?.parent).toBeNull();
    act(() => useSmartAlbumEditor.getState().close());
  });

  it("follows catalog writes: new counts, and the shown smart album shows its matches now", async () => {
    vi.useFakeTimers();
    try {
      albums = [...albums, smart];
      useLibraryStore.setState({ activeAlbumId: "session-album:s" });
      const select = await renderTool();
      albums = albums.map((a) => (a.id === "s" ? { ...a, count: 3 } : a));
      // The count follows a write at once, without waiting for a pause.
      await act(async () => catalogChanged());
      expect(screen.getByText("Five stars").closest("button")!.textContent).toContain("3");
      expect(select).toHaveBeenCalledTimes(1);
      expect(select).toHaveBeenCalledWith("session-album:s", "Five stars", expect.any(Array), true);
      // Writes while a count runs: one more count after it, not one each.
      select.mockClear();
      act(() => {
        catalogChanged();
        catalogChanged();
        catalogChanged();
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(select).toHaveBeenCalledTimes(2);
      select.mockClear();
      // A plain album shown is not reloaded on every write.
      useLibraryStore.setState({ activeAlbumId: "session-album:p" });
      act(() => catalogChanged());
      await act(async () => {
        await vi.advanceTimersByTimeAsync(400);
      });
      expect(select).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});


it.each([
  ["Previous Import", "session_previous_import", { batch: { id: 3, kind: "sync", created_at: "", count: 1 }, paths: ["/job/a.arw"] }],
  ["Missing Photographs", "session_offline", [{ path: "/job/a.arw", volume: null, mtime: 1 }]],
])("shows translated context for a failed %s query", async (label, command, result) => {
  const base = mockInvoke.getMockImplementation()!;
  let fail = false;
  mockInvoke.mockImplementation(async (name, args) => {
    if (name !== command) return base(name, args);
    if (fail) throw new Error("EIO: catalog");
    return result;
  });
  await renderTool();
  fail = true;
  vi.mocked(toast.error).mockClear();
  await act(async () => { fireEvent.click(screen.getByText(label as string)); });
  const content = vi.mocked(toast.error).mock.calls[0][0];
  expect(typeof content).toBe("object");
  render(<>{content as import("react").ReactNode}</>);
  expect(screen.getByText("The operation could not be completed.")).toBeTruthy();
  expect(screen.getByText("Error: EIO: catalog").tagName).toBe("SMALL");
});
