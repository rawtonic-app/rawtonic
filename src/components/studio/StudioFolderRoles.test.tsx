// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * The folder tree's "Set as Capture/Selects/Output Folder" (G38, UT3
 * 2026-11-01): two actions where the Session Folders dialog needs four. The
 * item calls the same command the dialog calls, with the folder the user
 * right-clicked.
 */
import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import StudioNavigation from "./StudioNavigation";
import { ContextMenuProvider } from "../../context/ContextMenuContext";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { AppSettings } from "../ui/AppProperties";
import type { FolderTree } from "../../utils/folderTree";
import type { SessionFolder } from "../../utils/sessionFolders";
import type { FolderActions } from "./StudioNavigation";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => false, invoke: vi.fn(() => Promise.resolve()) }));
vi.mock("../../activity/notify", () => ({ notify: { error: vi.fn(), success: vi.fn() } }));

const folder = (path: string, children: FolderTree[] = []): FolderTree =>
  ({
    name: path.split("/").pop()!,
    path,
    isDir: true,
    imageCount: 0,
    hasSubdirs: children.length > 0,
    children,
  }) as FolderTree;

const tree = folder("/job", [folder("/job/Capture"), folder("/job/Selects", [folder("/job/Selects/Day 1")]), folder("/job/Output")]);
const catalogTree = folder("/catalog", [folder("/catalog/Photos")]);

const role = (name: string): SessionFolder => ({
  role: name as SessionFolder["role"],
  stored: name === "capture" ? "Capture" : name === "selects" ? "Selects" : "Output",
  path: `/job/${name === "capture" ? "Capture" : name === "selects" ? "Selects" : "Output"}`,
  inside: true,
  available: true,
});

const setRoleFolder = vi.fn();

const renderTree = (folderActions: Pick<FolderActions, "setRoleFolder">) =>
  render(
    <ContextMenuProvider>
      <StudioNavigation onNavigate={vi.fn()} folderActions={{ moveHere: vi.fn(), trash: vi.fn(), ...folderActions }} />
    </ContextMenuProvider>,
  );

/** The folder's row, by its title (the path). */
const row = (path: string) => screen.getAllByTitle(path)[0].parentElement!;
/** The newest menu's item (closing menus fade out). */
const item = (name: RegExp | string) => screen.getAllByRole("menuitem", { name }).at(-1)!;

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === "get_pinned_folder_trees" ? [] : undefined,
  );
  useSettingsStore.setState({ appSettings: { pinnedFolders: [] } as unknown as AppSettings });
  useLibraryStore.setState({
    sessionDirectory: "/job",
    rootPaths: ["/job"],
    currentFolderPath: "/job/Capture",
    multiSelectedPaths: [],
    folderTrees: [tree],
    pinnedFolderTrees: [],
    expandedFolders: new Set(["/job", "/job/Selects"]),
    isTreeLoading: false,
    sessionFolders: [role("capture"), role("selects"), role("output")],
    catalogFolders: null,
    catalogTrees: [],
  });
  globalThis.HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  globalThis.HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
});
afterEach(() => cleanup());

describe("set a folder's role from the folder tree (G38)", () => {
  it("offers all three roles and sets the right-clicked folder as the one chosen, through the dialog's command", () => {
    renderTree({ setRoleFolder });
    fireEvent.contextMenu(row("/job/Selects/Day 1"));
    expect(item("Set as Capture Folder")).toHaveProperty("disabled", false);
    expect(item("Set as Selects Folder")).toHaveProperty("disabled", false);
    fireEvent.click(item("Set as Selects Folder"));
    expect(setRoleFolder).toHaveBeenCalledWith("selects", "/job/Selects/Day 1");
    fireEvent.contextMenu(row("/job/Selects/Day 1"));
    fireEvent.click(item("Set as Output Folder"));
    expect(setRoleFolder).toHaveBeenCalledWith("output", "/job/Selects/Day 1");
    fireEvent.contextMenu(row("/job/Selects/Day 1"));
    fireEvent.click(item("Set as Capture Folder"));
    expect(setRoleFolder).toHaveBeenCalledWith("capture", "/job/Selects/Day 1");
  });

  it("does not offer a role a folder already has", () => {
    renderTree({ setRoleFolder });
    fireEvent.contextMenu(row("/job/Capture"));
    expect(item("Set as Capture Folder")).toHaveProperty("disabled", true);
    expect(item("Set as Output Folder")).toHaveProperty("disabled", false);
  });

  it("refuses the session folder itself instead of offering it", () => {
    renderTree({ setRoleFolder });
    fireEvent.contextMenu(row("/job"));
    expect(screen.queryAllByRole("menuitem", { name: /^Set as .* Folder$/ })).toHaveLength(0);
  });

  it("offers no role folders in a catalog, which keeps two different ones as its own (G40)", async () => {
    // A catalog's folders come by drive (catalog_folders).
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "catalog_folders"
        ? {
            catalog: true,
            volumes: [
              {
                key: "id:SYS",
                id: "SYS",
                name: "Macintosh HD",
                kind: "startup",
                mount_point: "/",
                online: true,
                free_bytes: null,
                total_bytes: null,
                folders: [],
                roots: ["/catalog"],
              },
            ],
          }
        : [],
    );
    useLibraryStore.setState({
      sessionDirectory: "/catalog",
      rootPaths: ["/catalog"],
      folderTrees: [catalogTree],
      sessionFolders: [],
      expandedFolders: new Set(["/catalog"]),
    });
    renderTree({ setRoleFolder });
    // The folder list is a catalog's once its drives are read.
    await waitFor(() => expect(document.querySelector("[data-volume]")).toBeTruthy());
    fireEvent.contextMenu(row("/catalog/Photos"));
    expect(screen.queryAllByRole("menuitem", { name: /^Set as .* Folder$/ })).toHaveLength(0);
    expect(screen.getAllByRole("menuitem", { name: /role folders belong to a session/ }).at(-1)).toHaveProperty(
      "disabled",
      true,
    );
  });

});

describe("the session workspace sets the role folder", () => {
  it("runs the same command Session Folders runs, with the folder and no move", async () => {
    const { default: SessionWorkspace } = await import("../session/SessionWorkspace");
    const change = {
      folder: { ...role("output"), path: "/job/Selects/Day 1", stored: "Selects/Day 1" },
      previous: role("output"),
    };
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "session_set_folder") return change;
      if (command === "session_summary") return { directory: "/job", manifest: { name: "Job", mode: "session", capture: "Capture", output: "Output" }, folders: [change.folder], assets: 0, picked: 0, rated: 0, xmp_pending: 0 };
      if (command === "session_flags") return {};
      return { path: "/job", children: [] };
    });
    useLibraryStore.setState({ sessionDirectory: "/job" });
    render(<SessionWorkspace onNavigate={vi.fn(async () => {})} />);
    const { requestSetRoleFolder } = await import("../../utils/sessionFolders");
    requestSetRoleFolder({ role: "output", folder: "/job/Selects/Day 1" });
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("session_set_folder", {
        directory: "/job",
        role: "output",
        path: "/job/Selects/Day 1",
        moveContents: false,
      }),
    );
  });
});
