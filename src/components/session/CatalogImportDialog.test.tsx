import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { openDialog } from "../../utils/fileDialog";
import { flushBeforeQuit } from "../../utils/quit";
import { useActivityStore } from "../../store/useActivityStore";
import CatalogImportDialog, { type CatalogReview, runCatalogImport } from "./CatalogImportDialog";
import { getUndoLabel, undo, useCatalogUndoStore } from "../../utils/appUndo";

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
  name: "Laptop", new: 1, changed: 3, unchanged: 5, leftOut: 0,
  photos: [
    { id: "n1", target: null, matchedBy: null, status: "new", changes: [], side: null, path: "/card/d.arw", available: true },
    { id: "a", target: "a", matchedBy: "id", status: "changed", changes: ["edit", "metadata"], side: "there", path: "/s/a.arw", available: true },
    { id: "b", target: "b", matchedBy: "sha256", status: "changed", changes: ["edit"], side: "here", path: "/s/b.arw", available: true },
    // UXR3: a rating changed here after the export (and something there).
    { id: "c", target: "c", matchedBy: "id", status: "changed", changes: ["metadata"], side: "both", path: "/s/c.arw", available: true },
  ],
};

async function opened() {
  const user = userEvent.setup();
  vi.mocked(openDialog).mockResolvedValueOnce("/laptop/Job");
  vi.mocked(invoke).mockImplementation(async (command: string) => (command === "library_import_catalog_review" ? review : undefined));
  const onClose = vi.fn();
  render(<CatalogImportDialog directory="/Main" onClose={onClose} onDone={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: "Catalog to import from" }));
  await screen.findByText(/“Laptop”: 1 new, 3 changed, 5 unchanged/);
  return { user, onClose };
}

it("says which catalog changed each photo; Replace only for photos changed only in the other", async () => {
  await opened();
  expect(invoke).toHaveBeenCalledWith("library_import_catalog_review", { target: "/Main", other: "/laptop/Job" });
  expect(screen.getByText("d.arw")).toBeTruthy();
  expect(screen.getByText("edit, metadata · changed in the other catalog")).toBeTruthy();
  expect(screen.getByText("edit · changed in this catalog")).toBeTruthy();
  expect(screen.getByText("metadata · changed in both catalogs")).toBeTruthy();
  const run = "For photos changed only in the other catalog";
  expect((screen.getByRole("combobox", { name: run }) as HTMLSelectElement).value).toBe("replace");
  expect((screen.getByRole("combobox", { name: "What to do with a.arw" }) as HTMLSelectElement).value).toBe("replace");
  expect((screen.getByRole("combobox", { name: "What to do with b.arw" }) as HTMLSelectElement).value).toBe("keep");
  expect((screen.getByRole("combobox", { name: "What to do with c.arw" }) as HTMLSelectElement).value).toBe("keep");
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
      overrides: { a: "virtualCopy", b: "keep", c: "keep" }, includeNew: true, beforeSnapshot: "Before import from Laptop",
    },
  }));
  expect(vi.mocked(flushBeforeQuit).mock.invocationCallOrder[0])
    .toBeLessThan(vi.mocked(invoke).mock.invocationCallOrder.at(-1)!);
  expect(onClose).toHaveBeenCalled();
});

it("the run's choice is for photos changed only in the other catalog; newer work here stays kept", async () => {
  const { user } = await opened();
  await user.selectOptions(screen.getByRole("combobox", { name: "What to do with b.arw" }), "replace");
  await user.selectOptions(screen.getByRole("combobox", { name: "For photos changed only in the other catalog" }), "virtualCopy");
  const value = (name: string) => (screen.getByRole("combobox", { name }) as HTMLSelectElement).value;
  expect([value("What to do with a.arw"), value("What to do with b.arw"), value("What to do with c.arw")])
    .toEqual(["virtualCopy", "keep", "keep"]);
  await user.click(screen.getByRole("checkbox", { name: "Import new photos" }));
  await user.click(screen.getByRole("button", { name: "Import" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("library_import_catalog", expect.objectContaining({
    options: expect.objectContaining({ changed: "virtualCopy", overrides: { b: "keep", c: "keep" }, includeNew: false, newPhotos: { mode: "reference" } }),
  })));
});

it("one Undo puts back the metadata the import replaced", async () => {
  useCatalogUndoStore.setState({ past: [], future: [] });
  const receipt = { asset: "a", before: "{}", after: '{"rating":2}', xmp_hash: null, xmp_status: "synced", from_camera: false };
  vi.mocked(invoke).mockImplementation(async (command: string) =>
    command === "library_import_catalog"
      ? { added: 0, replaced: 1, virtualCopies: 0, kept: 0, albumsCreated: 0, unchanged: 0, originalsCopied: 0, offline: 0,
          missingResources: 0, leftOut: 0, undo: [receipt] }
      : undefined);
  const options = { newPhotos: { mode: "reference" as const }, changed: null, overrides: {}, includeNew: true, beforeSnapshot: "Before" };
  await runCatalogImport("/Main", "/laptop/Job", "Laptop", options, vi.fn().mockResolvedValue(undefined));
  expect(getUndoLabel()).toBe("Undo Import from Catalog");
  await undo();
  expect(invoke).toHaveBeenCalledWith("library_import_catalog_travel", { target: "/Main", steps: [receipt], undo: true });
  expect(useCatalogUndoStore.getState().future).toHaveLength(1);
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
  const options = { newPhotos: { mode: "reference" as const }, changed: null, overrides: {}, includeNew: true, beforeSnapshot: "Before" };
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
