import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import SessionFoldersDialog, { type FolderChange } from "./SessionFoldersDialog";
import { openDialog } from "../../utils/fileDialog";
import {
  openableFolderPath,
  outsideFolders,
  registerContentsMover,
  sessionFolderPath,
  sessionFoldersOrDefault,
  type SessionFolder,
} from "../../utils/sessionFolders";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../../utils/fileDialog", () => ({ openDialog: vi.fn() }));
const mover = vi.fn();

const folders: SessionFolder[] = [
  { role: "capture", stored: "Capture", path: "/Jobs/Job/Capture", inside: true, available: true, recreated: true },
  { role: "selects", stored: "/Volumes/Share/Selects", path: "/Volumes/Share/Selects", inside: false, available: true, recreated: false },
  { role: "output", stored: "/Volumes/Away/Output", path: "/Volumes/Away/Output", inside: false, available: false, recreated: false },
];

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(openDialog).mockReset();
  mover.mockReset();
  registerContentsMover(mover);
});
afterEach(cleanup);

const row = (role: string) => document.querySelector(`[data-role="${role}"]`) as HTMLElement;

it("shows each folder, where it is, and a folder whose drive is away", async () => {
  vi.mocked(invoke).mockResolvedValue(folders);
  render(<SessionFoldersDialog directory="/Jobs/Job" onChanged={vi.fn()} onClose={vi.fn()} />);
  expect(invoke).toHaveBeenCalledWith("session_folders", { directory: "/Jobs/Job" });
  await screen.findByText("/Jobs/Job/Capture");
  expect(within(row("capture")).getByText("Inside the session")).toBeTruthy();
  expect(within(row("selects")).getByText("Outside the session")).toBeTruthy();
  const output = row("output");
  expect(within(output).getByText(/Not available: the drive it is on is not connected/)).toBeTruthy();
  expect(within(output).getByRole("alert").textContent).toContain("Nothing is exported until the folder is chosen again.");
  // Nothing moves unless asked.
  expect(screen.getByText(/Changing a folder moves no files/)).toBeTruthy();
});

it("changes a folder without moving files by default", async () => {
  const changed: FolderChange = {
    folder: { role: "capture", stored: "/Volumes/Card/Job", path: "/Volumes/Card/Job", inside: false, available: true, recreated: false },
    previous: folders[0],
  };
  vi.mocked(invoke).mockImplementation(async (command) => (command === "session_folders" ? folders : changed));
  vi.mocked(openDialog).mockResolvedValue("/Volumes/Card/Job");
  const onChanged = vi.fn();
  render(<SessionFoldersDialog directory="/Jobs/Job" onChanged={onChanged} onClose={vi.fn()} />);
  await screen.findByText("/Jobs/Job/Capture");
  fireEvent.click(screen.getByRole("button", { name: "Change the Capture folder" }));
  await waitFor(() => expect(onChanged).toHaveBeenCalledWith(changed));
  expect(invoke).toHaveBeenCalledWith("session_set_folder", {
    directory: "/Jobs/Job",
    role: "capture",
    path: "/Volumes/Card/Job",
    moveContents: false,
  });
  expect(within(row("capture")).getByText("/Volumes/Card/Job")).toBeTruthy();
  expect(mover).not.toHaveBeenCalled();
});

it("moves the contents when asked and reports a move that failed", async () => {
  const changed: FolderChange = {
    folder: { role: "selects", stored: "Picks", path: "/Jobs/Job/Picks", inside: true, available: true, recreated: false },
    previous: folders[1],
  };
  vi.mocked(invoke).mockImplementation(async (command) => (command === "session_folders" ? folders : changed));
  vi.mocked(openDialog).mockResolvedValue("/Jobs/Job/Picks");
  mover.mockResolvedValue({ items: 3, operations: 1 });
  render(<SessionFoldersDialog directory="/Jobs/Job" onChanged={vi.fn()} onClose={vi.fn()} />);
  await screen.findByText("/Jobs/Job/Capture");
  fireEvent.click(screen.getByRole("checkbox", { name: "Move the contents to the new folder" }));
  expect(screen.getByText(/Edit ▸ Undo puts them back/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Change the Selects folder" }));
  await screen.findByText("Moved 3 items.");
  expect(invoke).toHaveBeenCalledWith("session_set_folder", expect.objectContaining({ role: "selects", moveContents: true }));
  // Through the Library's file operations (saved edits, Undo, paths followed).
  expect(mover).toHaveBeenCalledWith("/Volumes/Share/Selects", "/Jobs/Job/Picks");

  mover.mockResolvedValue({ items: 0, operations: 0, error: "B.RAF is already in Picks" });
  fireEvent.click(screen.getByRole("button", { name: "Change the Selects folder" }));
  await screen.findByText(/were not all moved: B.RAF is already in Picks/);
});

it("shows a refused folder and keeps the dialog open", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "session_folders") return folders;
    throw "/Jobs/Job/Trash keeps the session's own data and cannot be its Capture folder";
  });
  vi.mocked(openDialog).mockResolvedValue("/Jobs/Job/Trash");
  const onChanged = vi.fn();
  render(<SessionFoldersDialog directory="/Jobs/Job" onChanged={onChanged} onClose={vi.fn()} />);
  await screen.findByText("/Jobs/Job/Capture");
  fireEvent.click(screen.getByRole("button", { name: "Change the Capture folder" }));
  await screen.findByText(/cannot be its Capture folder/);
  expect(onChanged).not.toHaveBeenCalled();
  expect(within(row("capture")).getByText("/Jobs/Job/Capture")).toBeTruthy();
});

it("a cancelled folder picker changes nothing", async () => {
  vi.mocked(invoke).mockResolvedValue(folders);
  vi.mocked(openDialog).mockResolvedValue(null);
  render(<SessionFoldersDialog directory="/Jobs/Job" onChanged={vi.fn()} onClose={vi.fn()} />);
  await screen.findByText("/Jobs/Job/Capture");
  fireEvent.click(screen.getByRole("button", { name: "Change the Output folder" }));
  await waitFor(() => expect(openDialog).toHaveBeenCalled());
  expect(invoke).toHaveBeenCalledTimes(1);
});

it("finds a session's folders in its summary, older summaries included", () => {
  const summary = { directory: "/Jobs/Job/", folders, manifest: { capture: "Capture" } };
  expect(sessionFolderPath(summary, "selects")).toBe("/Volumes/Share/Selects");
  expect(sessionFolderPath({ directory: "/Jobs/Job/", manifest: { capture: "Shoot" } }, "capture")).toBe("/Jobs/Job/Shoot");
  expect(sessionFolderPath({ directory: "/Jobs/Job", manifest: { capture: "Capture" } }, "output")).toBe("/Jobs/Job/Output");
  // Only folders outside that are there are listed beside the session.
  expect(outsideFolders(folders)).toEqual(["/Volumes/Share/Selects"]);
  expect(outsideFolders(undefined)).toEqual([]);
});

it("a folder whose parent was renamed is reported and its old path is never offered", async () => {
  // Capture inside the session, under a parent that was renamed: it is not
  // there, and it is not the app's own default to make again (G38).
  const renamed: SessionFolder[] = [
    { role: "capture", stored: "Shoot/Day 1", path: "/Jobs/Job/Shoot/Day 1", inside: true, available: false, recreated: false },
    folders[1],
    folders[2],
  ];
  vi.mocked(invoke).mockResolvedValue(renamed);
  vi.mocked(openDialog).mockResolvedValue(null);
  render(<SessionFoldersDialog directory="/Jobs/Job" onChanged={vi.fn()} onClose={vi.fn()} />);
  await screen.findByText("/Jobs/Job/Shoot/Day 1");
  const capture = row("capture");
  expect(within(capture).getByText(/a folder above it was renamed or moved/)).toBeTruthy();
  expect(within(capture).getByRole("alert").textContent).toContain("Nothing is captured or imported until the folder is chosen again.");
  fireEvent.click(screen.getByRole("button", { name: "Change the Capture folder" }));
  await waitFor(() => expect(openDialog).toHaveBeenCalled());
  // The picker does not open at the old path (choosing it there is not in
  // the test's hands, but it is never the default).
  expect(vi.mocked(openDialog).mock.calls[0][0]).toMatchObject({ defaultPath: "/Jobs/Job" });
  expect(invoke).toHaveBeenCalledTimes(1);
  // The capture row is never the place a session opens at.
  expect(openableFolderPath(renamed, "capture", "/Jobs/Job/Capture")).toBeNull();
  expect(openableFolderPath(renamed, "output", "/Jobs/Job/Output")).toBeNull();
  // A folder the app gave the session is opened at its place (made again).
  const given = openableFolderPath([folders[0]], "capture", "/Jobs/Job/Capture");
  expect(given).toBe("/Jobs/Job/Capture");
  // An older summary (before G38) still shows its three folders.
  expect(sessionFoldersOrDefault("/Jobs/Job/", undefined).map((f) => f.path)).toEqual([
    "/Jobs/Job/Capture",
    "/Jobs/Job/Selects",
    "/Jobs/Job/Output",
  ]);
});
