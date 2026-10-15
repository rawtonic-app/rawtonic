import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { flushBeforeQuit } from "../../utils/quit";
import LibraryTransferDialog from "./LibraryTransferDialog";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => false, invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/api/path", () => ({ join: async (...parts: string[]) => parts.join("/") }));
vi.mock("../../utils/quit", () => ({ flushBeforeQuit: vi.fn(), registerQuitFlush: vi.fn(() => vi.fn()) }));
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
  HTMLDialogElement.prototype.close = function () { this.removeAttribute("open"); };
  vi.mocked(flushBeforeQuit).mockResolvedValue([]);
  vi.mocked(open).mockResolvedValue("/drive");
  vi.mocked(invoke).mockResolvedValue({ photographs: 42 });
});

it("flushes edits and creates a new full backup without opening it for editing", async () => {
  const user = userEvent.setup();
  const onOpen = vi.fn();
  render(<LibraryTransferDialog purpose="backup" directory="/photos" name="Travel" onClose={vi.fn()} onOpen={onOpen} />);
  await user.click(screen.getByRole("button", { name: "Destination parent folder" }));
  const folder = screen.getByRole("textbox") as HTMLInputElement;
  expect(folder.value).toMatch(/^Travel-backup-\d{4}-/);
  await user.click(screen.getByRole("button", { name: /Create full backup/ }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("library_copy", {
    source: "/photos", destination: `/drive/${folder.value}`, purpose: "backup",
  }));
  expect(vi.mocked(flushBeforeQuit).mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(invoke).mock.invocationCallOrder[0]);
  await screen.findByText(/42 photographs/);
  expect(screen.queryByRole("button", { name: /Open copied library/ })).toBeNull();
  expect(onOpen).not.toHaveBeenCalled();
});

it("reports edit resources that were already missing without failing the copy", async () => {
  const user = userEvent.setup();
  vi.mocked(invoke).mockResolvedValue({ photographs: 3, missing_resources: 2, missing_current_resources: 1 });
  render(<LibraryTransferDialog purpose="backup" directory="/photos" onClose={vi.fn()} onOpen={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: "Destination parent folder" }));
  await user.click(screen.getByRole("button", { name: /Create full backup/ }));
  await screen.findByText(/3 photographs/);
  expect(screen.getByText(/2 LUT, profile or mask files .* \(1 of them used by current edits\)/)).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
});

it("blocks the snapshot when pending edits could not be saved", async () => {
  const user = userEvent.setup();
  vi.mocked(flushBeforeQuit).mockResolvedValue(["image.arw"]);
  render(<LibraryTransferDialog purpose="transfer" directory="/photos" onClose={vi.fn()} onOpen={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: "Destination parent folder" }));
  await user.click(screen.getByRole("button", { name: /Create portable copy/ }));
  expect((await screen.findByRole("alert")).textContent).toContain("image.arw");
  expect(invoke).not.toHaveBeenCalled();
});

it("restores the chosen backup into a new folder and opens only the finished copy", async () => {
  const user = userEvent.setup();
  const onOpen = vi.fn(async () => {});
  const close = vi.fn();
  vi.mocked(open).mockResolvedValueOnce("/backup").mockResolvedValueOnce("/drive");
  render(<LibraryTransferDialog purpose="restore" directory="/current" onClose={close} onOpen={onOpen} />);
  await user.click(screen.getByRole("button", { name: "Backup folder" }));
  await user.click(screen.getByRole("button", { name: "Destination parent folder" }));
  const folder = screen.getByRole("textbox");
  await user.clear(folder);
  await user.type(folder, "Restored");
  await user.click(screen.getByRole("button", { name: /Restore backup/ }));
  await screen.findByText(/42 photographs/);
  expect(invoke).toHaveBeenCalledWith("library_copy", { source: "/backup", destination: "/drive/Restored", purpose: "restore" });
  expect(onOpen).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: /Open copied library/ }));
  await waitFor(() => expect(close).toHaveBeenCalled());
  expect(onOpen).toHaveBeenCalledWith("/drive/Restored");
});
