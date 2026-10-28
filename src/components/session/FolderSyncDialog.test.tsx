import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import FolderSyncDialog from "./FolderSyncDialog";
import { samplePlan } from "../../utils/folderSync.fixture";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => false, invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
});

const applied = { moved: [["/lib/Capture/A.ARW", "/lib/Capture/Day 2/A.ARW"]], added: [], removed: [], skipped: [] };

function mockInvoke() {
  vi.mocked(invoke).mockImplementation(async (command: string) => {
    if (command === "folder_sync_scan") return samplePlan();
    if (command === "folder_sync_apply") return applied;
    return null;
  });
}

const checkbox = (name: string) => screen.getByRole("checkbox", { name }) as HTMLInputElement;

it("ticks verified moves and new files, leaves probable moves, copies and removals to the user", async () => {
  mockInvoke();
  render(<FolderSyncDialog directory="/lib" folder="/lib/Capture" onClose={vi.fn()} onApplied={vi.fn()} />);
  await screen.findByText("Moved or renamed (verified)");
  expect(invoke).toHaveBeenCalledWith("folder_sync_scan", { directory: "/lib", folder: "/lib/Capture" });
  expect(checkbox("/lib/Capture/Day 2/A.ARW").checked).toBe(true);
  expect(checkbox("/lib/Capture/N.ARW").checked).toBe(true);
  expect(checkbox("/lib/Capture/B2.ARW").checked).toBe(false);
  expect(checkbox("/lib/Capture/C copy.ARW").checked).toBe(false);
  expect(checkbox("/lib/Capture/R.ARW").checked).toBe(false);
  // Unavailable photos are listed without a checkbox.
  const unavailable = screen.getByRole("region", { name: "Not available" });
  expect(within(unavailable).queryByRole("checkbox")).toBeNull();
  expect(within(unavailable).getByText("on “Photos”")).toBeTruthy();
});

it("needs a choice for every contested file before it applies, then sends exactly what is ticked", async () => {
  mockInvoke();
  const user = userEvent.setup();
  const onApplied = vi.fn();
  render(<FolderSyncDialog directory="/lib" folder={null} onClose={vi.fn()} onApplied={onApplied} />);
  await screen.findByText("Needs your choice");
  expect(screen.getByText("The whole library")).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Synchronize Library" })).toBeTruthy();
  const apply = screen.getByRole("button", { name: /Apply/ }) as HTMLButtonElement;
  expect(apply.disabled).toBe(true);
  await user.click(screen.getByRole("radio", { name: "It is old/X.ARW, moved (verified)" }));
  expect(apply.disabled).toBe(false);
  expect(apply.textContent).toBe("Apply (3)");
  // The removal only when the user ticks it.
  await user.click(checkbox("/lib/Capture/R.ARW"));
  expect(apply.textContent).toBe("Apply (4)");
  await user.click(apply);
  await waitFor(() => expect(onApplied).toHaveBeenCalledWith(applied));
  const call = vi.mocked(invoke).mock.calls.find(([command]) => command === "folder_sync_apply");
  expect(call?.[1]).toEqual({
    directory: "/lib",
    actions: [
      expect.objectContaining({ kind: "move", assetId: "a" }),
      expect.objectContaining({ kind: "add", path: "/lib/Capture/N.ARW" }),
      { kind: "remove", assetId: "r", path: "/lib/Capture/R.ARW" },
      expect.objectContaining({ kind: "move", assetId: "x" }),
    ],
  });
  expect(await screen.findByText("Moved 1 · Added 0 · Removed 0")).toBeTruthy();
});

it("a group's checkbox ticks all its rows", async () => {
  mockInvoke();
  const user = userEvent.setup();
  render(<FolderSyncDialog directory="/lib" folder={null} onClose={vi.fn()} onApplied={vi.fn()} />);
  await screen.findByText("Probably moved or renamed");
  await user.click(checkbox("Select all: Probably moved or renamed"));
  expect(checkbox("/lib/Capture/B2.ARW").checked).toBe(true);
  await user.click(checkbox("Select all: Moved or renamed (verified)"));
  expect(checkbox("/lib/Capture/Day 2/A.ARW").checked).toBe(false);
});

it("closes when the scan is cancelled and shows a failed scan", async () => {
  const onClose = vi.fn();
  vi.mocked(invoke).mockRejectedValueOnce("cancelled");
  render(<FolderSyncDialog directory="/lib" folder="/lib/Capture" onClose={onClose} onApplied={vi.fn()} />);
  await waitFor(() => expect(onClose).toHaveBeenCalled());
  cleanup();
  vi.mocked(invoke).mockRejectedValueOnce("/lib/Gone is not there.");
  render(<FolderSyncDialog directory="/lib" folder="/lib/Gone" onClose={vi.fn()} onApplied={vi.fn()} />);
  expect((await screen.findByRole("alert")).textContent).toContain("is not there");
});

it("shows paths relative to the scanned folder, whole paths elsewhere and in tooltips", async () => {
  mockInvoke();
  render(<FolderSyncDialog directory="/lib" folder="/lib/Capture" onClose={vi.fn()} onApplied={vi.fn()} />);
  const moved = await screen.findByRole("region", { name: "Moved or renamed (verified)" });
  expect(within(moved).getByText("A.ARW").getAttribute("title")).toBe("/lib/Capture/A.ARW");
  expect(within(moved).getByText("Day 2/A.ARW").getAttribute("title")).toBe("/lib/Capture/Day 2/A.ARW");
  expect(screen.getByText("X.ARW", { selector: "legend" })).toBeTruthy();
  // Outside the scanned folder: the whole path.
  expect(within(screen.getByRole("region", { name: "Not available" })).getByText("/Volumes/Photos/U.ARW")).toBeTruthy();
});

it("offers only Close when everything is in sync", async () => {
  const empty = { ...samplePlan(), verifiedMoves: [], probableMoves: [], newFiles: [], copies: [], contested: [], removed: [], unavailable: [] };
  vi.mocked(invoke).mockResolvedValueOnce(empty);
  const onClose = vi.fn();
  const user = userEvent.setup();
  render(<FolderSyncDialog directory="/lib" folder="/lib/Capture" onClose={onClose} onApplied={vi.fn()} />);
  await screen.findByText(/^Everything is in sync/);
  expect(screen.queryByRole("button", { name: /Apply/ })).toBeNull();
  const close = screen.getAllByRole("button", { name: "Close" });
  await user.click(close[close.length - 1]);
  expect(onClose).toHaveBeenCalled();
});

it("lists the rows of a group in natural order", async () => {
  const plan = samplePlan();
  const file = (name: string) => ({ path: `/lib/Capture/${name}`, identity: plan.newFiles[0].identity, copyOf: [] });
  vi.mocked(invoke).mockResolvedValueOnce({ ...plan, newFiles: ["copy_10.ARW", "copy_2.ARW", "copy_1.ARW"].map(file) });
  render(<FolderSyncDialog directory="/lib" folder="/lib/Capture" onClose={vi.fn()} onApplied={vi.fn()} />);
  const added = await screen.findByRole("region", { name: "New photos" });
  expect(within(added).getAllByRole("listitem").map((row) => row.textContent)).toEqual([
    "copy_1.ARW",
    "copy_2.ARW",
    "copy_10.ARW",
  ]);
});

it("does not call a folder on a disconnected drive in sync", async () => {
  const offline = { ...samplePlan(), files: 0, verifiedMoves: [], probableMoves: [], newFiles: [], copies: [], contested: [], removed: [] };
  vi.mocked(invoke).mockResolvedValueOnce(offline);
  render(<FolderSyncDialog directory="/lib" folder="/Volumes/Photos" onClose={vi.fn()} onApplied={vi.fn()} />);
  await screen.findByText("Nothing to change: 0 photo files looked at. The photos below could not be checked.");
  expect(screen.queryByText(/in sync/)).toBeNull();
  expect(within(screen.getByRole("region", { name: "Not available" })).getByText("on “Photos”")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /Apply/ })).toBeNull();
});

it("keeps a scan failure below a translated summary", async () => {
  vi.mocked(invoke).mockRejectedValueOnce("EACCES: folder");
  render(<FolderSyncDialog directory="/lib" folder={null} onClose={vi.fn()} onApplied={vi.fn()} />);
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toContain("The operation could not be completed.");
  expect(alert.querySelector("small")?.textContent).toBe("EACCES: folder");
});
