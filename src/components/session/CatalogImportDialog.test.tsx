import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { openDialog } from "../../utils/fileDialog";
import { flushBeforeQuit } from "../../utils/quit";
import { useActivityStore } from "../../store/useActivityStore";
import CatalogImportDialog, { type CatalogReview, runCatalogImport } from "./CatalogImportDialog";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => false, invoke: vi.fn() }));
vi.mock("../../utils/fileDialog", () => ({ openDialog: vi.fn() }));
vi.mock("react-toastify", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), info: vi.fn(), dismiss: vi.fn() }) }));
vi.mock("../../utils/quit", () => ({ flushBeforeQuit: vi.fn(), registerQuitFlush: vi.fn(() => vi.fn()) }));
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  useActivityStore.setState({ entries: [] });
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
  HTMLDialogElement.prototype.close = function () { this.removeAttribute("open"); };
  vi.mocked(flushBeforeQuit).mockResolvedValue([]);
});

const review: CatalogReview = {
  name: "Laptop", new: 1, changed: 2, unchanged: 5, leftOut: 0,
  photos: [
    { id: "n1", target: null, matchedBy: null, status: "new", changes: [], changedHereOnly: false, path: "/card/d.arw", available: true },
    { id: "a", target: "a", matchedBy: "id", status: "changed", changes: ["edit", "metadata"], changedHereOnly: false, path: "/s/a.arw", available: true },
    { id: "b", target: "b", matchedBy: "sha256", status: "changed", changes: ["edit"], changedHereOnly: true, path: "/s/b.arw", available: true },
  ],
};

async function opened() {
  const user = userEvent.setup();
  vi.mocked(openDialog).mockResolvedValueOnce("/laptop/Job");
  vi.mocked(invoke).mockImplementation(async (command: string) => (command === "library_import_catalog_review" ? review : undefined));
  const onClose = vi.fn();
  render(<CatalogImportDialog directory="/Main" onClose={onClose} onDone={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: "Catalog to import from" }));
  await screen.findByText(/“Laptop”: 1 new, 2 changed, 5 unchanged/);
  return { user, onClose };
}

it("reviews the other catalog: new and changed photos, Keep current for photos changed only here", async () => {
  await opened();
  expect(invoke).toHaveBeenCalledWith("library_import_catalog_review", { target: "/Main", other: "/laptop/Job" });
  expect(screen.getByText("d.arw")).toBeTruthy();
  expect(screen.getByText("edit, metadata")).toBeTruthy();
  expect((screen.getByRole("combobox", { name: "For changed photos" }) as HTMLSelectElement).value).toBe("replace");
  expect((screen.getByRole("combobox", { name: "What to do with a.arw" }) as HTMLSelectElement).value).toBe("replace");
  expect((screen.getByRole("combobox", { name: "What to do with b.arw" }) as HTMLSelectElement).value).toBe("keep");
  expect(screen.getByText(/changed only in this catalog/)).toBeTruthy();
});

it("imports with per-photo choices and copies of new originals, after saving edits", async () => {
  const { user, onClose } = await opened();
  await user.selectOptions(screen.getByRole("combobox", { name: "What to do with a.arw" }), "virtualCopy");
  vi.mocked(openDialog).mockResolvedValueOnce("/photos/Imported");
  await user.click(screen.getByRole("radio", { name: "Copy their originals to a folder…" }));
  await screen.findByText("/photos/Imported");
  await user.click(screen.getByRole("button", { name: "Import" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("library_import_catalog", {
    target: "/Main", other: "/laptop/Job",
    options: {
      newPhotos: { mode: "copy", folder: "/photos/Imported" }, changed: "replace",
      overrides: { a: "virtualCopy", b: "keep" }, includeNew: true,
    },
  }));
  expect(vi.mocked(flushBeforeQuit).mock.invocationCallOrder[0])
    .toBeLessThan(vi.mocked(invoke).mock.invocationCallOrder.at(-1)!);
  expect(onClose).toHaveBeenCalled();
});

it("one choice for the run applies to every changed photo", async () => {
  const { user } = await opened();
  await user.selectOptions(screen.getByRole("combobox", { name: "For changed photos" }), "keep");
  const list = screen.getByRole("group", { name: /Changed photos/ });
  for (const select of within(list).getAllByRole("combobox").slice(1)) expect((select as HTMLSelectElement).value).toBe("keep");
  await user.click(screen.getByRole("checkbox", { name: "Import new photos" }));
  await user.click(screen.getByRole("button", { name: "Import" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("library_import_catalog", expect.objectContaining({
    options: expect.objectContaining({ changed: "keep", overrides: {}, includeNew: false, newPhotos: { mode: "reference" } }),
  })));
});

it("shows a review error and imports nothing", async () => {
  const user = userEvent.setup();
  vi.mocked(openDialog).mockResolvedValueOnce("/Main");
  vi.mocked(invoke).mockRejectedValue("Choose another catalog than the open one");
  render(<CatalogImportDialog directory="/Main" onClose={vi.fn()} onDone={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: "Catalog to import from" }));
  await screen.findByRole("alert");
  expect((screen.getByRole("button", { name: "Import" }) as HTMLButtonElement).disabled).toBe(true);
});

it("reports the result and refreshes the catalog; a failure says nothing was changed", async () => {
  vi.mocked(invoke).mockResolvedValue({
    added: 1, replaced: 2, virtualCopies: 0, kept: 1, albumsCreated: 1, unchanged: 5, originalsCopied: 0, offline: 0, missingResources: 0, leftOut: 0,
  });
  const onDone = vi.fn().mockResolvedValue(undefined);
  const options = { newPhotos: { mode: "reference" as const }, changed: null, overrides: {}, includeNew: true };
  await runCatalogImport("/Main", "/laptop/Job", "Laptop", options, onDone);
  const done = useActivityStore.getState().entries.find((entry) => entry.status === "done")!;
  expect(done.title).toBe("Imported from catalog “Laptop”");
  expect(done.detail).toMatch(/1 new photo added\n2 photos updated\n1 changed photo kept as it was\n5 unchanged photos skipped/);
  expect(onDone).toHaveBeenCalled();
  vi.mocked(invoke).mockRejectedValue("disk full");
  await runCatalogImport("/Main", "/laptop/Job", "Laptop", options, onDone);
  const failed = useActivityStore.getState().entries.find((entry) => entry.status === "failure")!;
  expect(failed.title).toMatch(/nothing was changed/);
  expect(failed.detail).toBe("disk full");
});
