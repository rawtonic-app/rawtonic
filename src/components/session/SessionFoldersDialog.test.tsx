import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import SessionFoldersDialog, { type FolderChange } from "./SessionFoldersDialog";
import { openDialog } from "../../utils/fileDialog";
import {
  outsideFolders,
  registerContentsMover,
  sessionFolderPath,
  type SessionFolder,
} from "../../utils/sessionFolders";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../../utils/fileDialog", () => ({ openDialog: vi.fn() }));
const mover = vi.fn();

const folders: SessionFolder[] = [
  { role: "capture", stored: "Capture", path: "/Jobs/Job/Capture", inside: true, available: true },
  { role: "selects", stored: "/Volumes/Share/Selects", path: "/Volumes/Share/Selects", inside: false, available: true },
  { role: "output", stored: "/Volumes/Away/Output", path: "/Volumes/Away/Output", inside: false, available: false },
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
  expect(within(output).getByText(/Not available/)).toBeTruthy();
  expect(within(output).getByRole("alert").textContent).toContain("Exports to the Output folder fail");
  // Nothing moves unless asked.
  expect(screen.getByText(/Changing a folder moves no files/)).toBeTruthy();
});

it("changes a folder without moving files by default", async () => {
  const changed: FolderChange = {
    folder: { role: "capture", stored: "/Volumes/Card/Job", path: "/Volumes/Card/Job", inside: false, available: true },
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
    folder: { role: "selects", stored: "Picks", path: "/Jobs/Job/Picks", inside: true, available: true },
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
