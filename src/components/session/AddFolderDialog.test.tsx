import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import AddFolderDialog from "./AddFolderDialog";
import UnaddedFolderBanner from "./UnaddedFolderBanner";
import { bannerDismissed, listenForAddFolder, requestAddFolder, type FolderPlan } from "./addFolder";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useActivityStore } from "../../store/useActivityStore";
import { IMPORTS_CHANGED } from "../../utils/folderSync";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const plan = (over: Partial<FolderPlan> = {}): FolderPlan => ({
  files: ["/Drive/Shoot/A.NEF", "/Drive/Shoot/A.JPG", "/Drive/Shoot/sub/B.NEF"],
  photographs: 2,
  companions: 0,
  registered: 0,
  unreadable: [],
  owned: [],
  truncated: false,
  ...over,
});

beforeEach(() => {
  localStorage.clear();
  vi.mocked(invoke).mockReset();
});
afterEach(cleanup);

const dialog = (catalog: boolean) => {
  const handlers = { onAdd: vi.fn(), onShow: vi.fn(), onCopy: vi.fn(), onClose: vi.fn() };
  render(<AddFolderDialog folder="/Drive/Shoot" directory="/Main" catalog={catalog} {...handlers} />);
  return handlers;
};

it("in a catalog, adds the folder's photographs where they are, subfolders by default", async () => {
  vi.mocked(invoke).mockImplementation(async (_command, args) => {
    const { subfolders } = args as { subfolders: boolean };
    return subfolders
      ? plan({ registered: 4, owned: [{ library: "/Drive/Shoot/Wedding", name: "Wedding Smith", count: 3 }] })
      : plan({ files: ["/Drive/Shoot/A.NEF"], photographs: 1 });
  });
  const { onAdd } = dialog(true);
  expect(invoke).toHaveBeenCalledWith("catalog_add_folder_scan", {
    directory: "/Main",
    folder: "/Drive/Shoot",
    subfolders: true,
  });
  await screen.findByText("2 photos to add");
  expect(screen.getByText("4 files already in the catalog are skipped.")).toBeTruthy();
  expect(
    screen.getByText("3 files belong to “Wedding Smith” and are not added: open that library to work on them."),
  ).toBeTruthy();
  expect(screen.getByText(/nothing is copied, moved or written beside them/)).toBeTruthy();

  const subfolders = screen.getByRole("checkbox", { name: "Include subfolders" }) as HTMLInputElement;
  expect(subfolders.checked).toBe(true);
  fireEvent.click(subfolders);
  await screen.findByText("1 photo to add");
  fireEvent.click(screen.getByRole("button", { name: "Add 1 photo" }));
  expect(onAdd).toHaveBeenCalledWith(["/Drive/Shoot/A.NEF"]);
});

it("in a catalog with nothing new, says so and only shows the folder", async () => {
  vi.mocked(invoke).mockResolvedValue(plan({ files: [], photographs: 0, registered: 3 }));
  const { onAdd, onShow } = dialog(true);
  await screen.findByText("No new photos: everything here is in the catalog already.");
  fireEvent.click(screen.getByRole("button", { name: "Show folder" }));
  expect(onShow).toHaveBeenCalled();
  expect(onAdd).not.toHaveBeenCalled();
});

it("in a project, says the folder is only shown and offers the verified copy", () => {
  const { onCopy, onShow, onClose } = dialog(false);
  expect(invoke).not.toHaveBeenCalled();
  expect(screen.getByText(/Shown here, not added — projects keep their own files/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Copy into this project…" }));
  expect(onCopy).toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Show folder" }));
  expect(onShow).toHaveBeenCalled();
  fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
  expect(onClose).toHaveBeenCalled();
});

it("names unreadable folders and JPEGs that join their RAWs", async () => {
  vi.mocked(invoke).mockResolvedValue(
    plan({ files: ["/Drive/Shoot/A.NEF", "/Drive/Shoot/A.JPG"], photographs: 0, companions: 1, unreadable: ["/Drive/Shoot/locked"] }),
  );
  const { onAdd } = dialog(true);
  await screen.findByText("No new photos.");
  expect(screen.getByText("1 JPEG joins its RAW already in the catalog.")).toBeTruthy();
  expect(screen.getByText("1 folder could not be read and was left out: /Drive/Shoot/locked")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Keep the JPEGs with their RAWs" }));
  expect(onAdd).toHaveBeenCalledWith(["/Drive/Shoot/A.NEF", "/Drive/Shoot/A.JPG"]);
});

it("reports a folder that cannot be read", async () => {
  vi.mocked(invoke).mockRejectedValue("Permission denied");
  dialog(true);
  await screen.findByText("The folder could not be read: Permission denied");
});

it("hands Add Folder to a listening library workspace, else not", () => {
  expect(requestAddFolder({ folder: "/x", show: async () => {} })).toBe(false);
  const seen = vi.fn();
  const stop = listenForAddFolder(seen);
  expect(requestAddFolder({ folder: "/x", show: async () => {} })).toBe(true);
  expect(seen).toHaveBeenCalledWith(expect.objectContaining({ folder: "/x" }));
  stop();
  expect(requestAddFolder({ folder: "/x", show: async () => {} })).toBe(false);
});

it("offers once to add a listed folder's photographs the catalog does not hold", async () => {
  useLibraryStore.setState({
    sessionDirectory: "/Main",
    catalogFolders: { catalog: true, volumes: [] },
    rootPaths: ["/Main", "/Drive/Shoot"],
    currentFolderPath: "/Drive/Shoot/sub",
  });
  vi.mocked(invoke).mockResolvedValue(plan({ photographs: 12 }));
  const opened = vi.fn();
  const stop = listenForAddFolder(opened);
  render(<UnaddedFolderBanner />);
  await screen.findByText(
    "12 photos in “Shoot” are shown here but not in the catalog: they are not counted, searched or exported.",
  );
  expect(invoke).toHaveBeenCalledWith("catalog_add_folder_scan", {
    directory: "/Main",
    folder: "/Drive/Shoot",
    subfolders: true,
  });
  fireEvent.click(screen.getByRole("button", { name: "Add these photos to the catalog" }));
  expect(opened).toHaveBeenCalledWith(expect.objectContaining({ folder: "/Drive/Shoot" }));
  stop();

  // Added: the next scan finds nothing and the banner goes.
  vi.mocked(invoke).mockResolvedValue(plan({ files: [], photographs: 0, registered: 12 }));
  act(() => {
    window.dispatchEvent(new Event(IMPORTS_CHANGED));
  });
  await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
});

it("is not shown while an import runs", async () => {
  useLibraryStore.setState({
    sessionDirectory: "/Main",
    catalogFolders: { catalog: true, volumes: [] },
    rootPaths: ["/Main", "/Drive/Shoot"],
    currentFolderPath: "/Drive/Shoot",
  });
  vi.mocked(invoke).mockResolvedValue(plan());
  act(() => {
    useActivityStore.getState().start({ id: "import:ingest", source: "import", title: "Adding" });
  });
  render(<UnaddedFolderBanner />);
  await waitFor(() => expect(invoke).toHaveBeenCalled());
  expect(screen.queryByRole("status")).toBeNull();
  act(() => useActivityStore.getState().finish("import:ingest"));
  await screen.findByRole("status");
});

it("Not now puts the banner away for that folder for good", async () => {
  useLibraryStore.setState({
    sessionDirectory: "/Main",
    catalogFolders: { catalog: true, volumes: [] },
    rootPaths: ["/Main", "/Drive/Shoot"],
    currentFolderPath: "/Drive/Shoot",
  });
  vi.mocked(invoke).mockResolvedValue(plan());
  render(<UnaddedFolderBanner />);
  fireEvent.click(await screen.findByRole("button", { name: "Not now" }));
  await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
  expect(bannerDismissed("/Main", "/Drive/Shoot")).toBe(true);
  cleanup();
  vi.mocked(invoke).mockClear();
  render(<UnaddedFolderBanner />);
  expect(invoke).not.toHaveBeenCalled();
});

it("shows no banner for the library's own folder or a project", () => {
  useLibraryStore.setState({
    sessionDirectory: "/Main",
    catalogFolders: { catalog: true, volumes: [] },
    rootPaths: ["/Main"],
    currentFolderPath: "/Main/Capture",
  });
  render(<UnaddedFolderBanner />);
  cleanup();
  useLibraryStore.setState({
    catalogFolders: { catalog: false, volumes: [] },
    rootPaths: ["/Main", "/Drive/Shoot"],
    currentFolderPath: "/Drive/Shoot",
  });
  render(<UnaddedFolderBanner />);
  expect(invoke).not.toHaveBeenCalled();
});
