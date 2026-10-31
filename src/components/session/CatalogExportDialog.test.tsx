import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import { openDialog } from "../../utils/fileDialog";
import { flushBeforeQuit } from "../../utils/quit";
import { useActivityStore } from "../../store/useActivityStore";
import CatalogExportDialog, { CATALOG_EXPORT_ACTIVITY, runCatalogExport } from "./CatalogExportDialog";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => false, invoke: vi.fn() }));
vi.mock("../../utils/fileDialog", () => ({ openDialog: vi.fn() }));
vi.mock("@tauri-apps/api/path", () => ({ join: async (...parts: string[]) => parts.join("/") }));
vi.mock("react-toastify", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), info: vi.fn(), dismiss: vi.fn() }) }));
vi.mock("../../utils/quit", () => ({ flushBeforeQuit: vi.fn(), registerQuitFlush: vi.fn(() => vi.fn()) }));
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  useActivityStore.setState({ entries: [] });
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
  HTMLDialogElement.prototype.close = function () { this.removeAttribute("open"); };
  vi.mocked(flushBeforeQuit).mockResolvedValue([]);
  vi.mocked(openDialog).mockResolvedValue("/laptop");
});

const receipt = { photographs: 2, originalsCopied: 0, offline: 0, previews: 2, missingResources: 0, missingCurrentResources: 0, leftOut: 0 };
/** The backend: nothing owned elsewhere unless `owned` says so. */
function backend(owned: { id: string; path: string; library: string }[] = [], total = 2) {
  vi.mocked(invoke).mockImplementation(async (command: string) =>
    command === "library_export_catalog_owned" ? { total, owned } : receipt);
}

it("exports the selection by reference with previews by default, after saving edits, and closes", async () => {
  const user = userEvent.setup();
  backend();
  const onClose = vi.fn();
  render(<CatalogExportDialog directory="/Main" name="Main" selected={["/s/a.arw", "/s/b.arw?vc=abcdef"]}
    shown={["/s/a.arw", "/s/b.arw", "/s/c.arw"]} onClose={onClose} onOpen={vi.fn()} />);
  expect((screen.getByRole("radio", { name: /Selected photos \(2\)/ }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("checkbox", { name: "Include original files" }) as HTMLInputElement).checked).toBe(false);
  expect((screen.getByRole("checkbox", { name: "Include previews" }) as HTMLInputElement).checked).toBe(true);
  const submit = screen.getByRole("button", { name: "Export 2 photos" });
  expect((submit as HTMLButtonElement).disabled).toBe(true);
  await user.click(screen.getByRole("button", { name: "Destination parent folder" }));
  const folder = screen.getByRole("textbox") as HTMLInputElement;
  await user.clear(folder);
  await user.type(folder, "Job");
  await user.click(submit);
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("library_export_catalog", {
    source: "/Main", paths: ["/s/a.arw", "/s/b.arw?vc=abcdef"], destination: "/laptop/Job",
    includeOriginals: false, includePreviews: true, leaveOutOwnedElsewhere: false,
  }));
  expect(vi.mocked(flushBeforeQuit).mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(invoke).mock.invocationCallOrder[0]);
  expect(onClose).toHaveBeenCalled();
});

it("can export what the folder or album shows, with originals", async () => {
  const user = userEvent.setup();
  backend();
  render(<CatalogExportDialog directory="/Main" name="Main" selected={[]} shown={["/s/a.arw", "/s/c.arw"]}
    onClose={vi.fn()} onOpen={vi.fn()} />);
  expect((screen.getByRole("radio", { name: /Selected photos \(0\)/ }) as HTMLInputElement).disabled).toBe(true);
  expect((screen.getByRole("radio", { name: /All photos shown \(2\)/ }) as HTMLInputElement).checked).toBe(true);
  await user.click(screen.getByRole("checkbox", { name: "Include original files" }));
  expect(screen.getByText(/copied into the catalog folder and verified/)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Destination parent folder" }));
  await user.click(screen.getByRole("button", { name: "Export 2 photos" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("library_export_catalog", expect.objectContaining({
    paths: ["/s/a.arw", "/s/c.arw"], includeOriginals: true,
  })));
});

it("does not export when edits could not be saved", async () => {
  const user = userEvent.setup();
  vi.mocked(flushBeforeQuit).mockResolvedValue(["a.arw"]);
  const onClose = vi.fn();
  render(<CatalogExportDialog directory="/Main" name="Main" selected={["/s/a.arw"]} shown={[]} onClose={onClose} onOpen={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: "Destination parent folder" }));
  await user.click(screen.getByRole("button", { name: "Export 1 photo" }));
  await screen.findByRole("alert");
  expect(invoke).not.toHaveBeenCalled();
  expect(onClose).not.toHaveBeenCalled();
});

it("shows progress and Cancel in the activity centre and the result with Open catalog", async () => {
  let finish: (value: unknown) => void = () => {};
  vi.mocked(invoke).mockImplementation((command: string) =>
    command === "library_export_catalog" ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve());
  const onOpen = vi.fn().mockResolvedValue(undefined);
  const done = runCatalogExport({ source: "/Main", paths: ["/a", "/b"], destination: "/laptop/Job", includeOriginals: true, includePreviews: true, leaveOutOwnedElsewhere: false }, onOpen);
  await waitFor(() => expect(useActivityStore.getState().entries[0]?.status).toBe("running"));
  const running = useActivityStore.getState().entries[0];
  expect(running.id).toBe(CATALOG_EXPORT_ACTIVITY);
  expect(running.progress).toEqual({ done: 0, total: 2 });
  await running.cancel!();
  expect(invoke).toHaveBeenCalledWith("library_export_catalog_cancel");
  finish({ ...receipt, originalsCopied: 2 });
  await done;
  const result = useActivityStore.getState().entries.find((entry) => entry.status === "done")!;
  expect(useActivityStore.getState().entries.some((entry) => entry.status === "running")).toBe(false);
  expect(result.title).toMatch(/Job.*2 photos/);
  expect(result.detail).toMatch(/2 originals copied and verified/);
  await result.actions![0].run();
  expect(onOpen).toHaveBeenCalledWith("/laptop/Job");
});

it("reports a failure and that nothing was created", async () => {
  vi.mocked(invoke).mockRejectedValue("Originals not available (1)");
  await runCatalogExport({ source: "/Main", paths: ["/a"], destination: "/laptop/Job", includeOriginals: true, includePreviews: false, leaveOutOwnedElsewhere: false }, vi.fn());
  const entry = useActivityStore.getState().entries[0];
  expect(entry.status).toBe("failure");
  expect(entry.title).toMatch(/nothing was created/);
  expect(entry.detail).toMatch(/Originals not available/);
});

it("photos another project owns are left out only after Export the others, and never all of them", async () => {
  const user = userEvent.setup();
  const owned = [{ id: "t", path: "/Other/Capture/t.arw", library: "/Projects/Wedding" }];
  backend(owned, 2);
  const onClose = vi.fn();
  render(<CatalogExportDialog directory="/Main" name="Main" selected={["/s/a.arw", "/Other/Capture/t.arw"]} shown={[]}
    onClose={onClose} onOpen={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: "Destination parent folder" }));
  await user.click(screen.getByRole("button", { name: "Export 2 photos" }));
  const notice = await screen.findByRole("alertdialog", { name: "1 photo belongs to another project or catalog" });
  expect(notice.textContent).toMatch(/latest edits are in Wedding/);
  expect(invoke).not.toHaveBeenCalledWith("library_export_catalog", expect.anything());
  await user.click(screen.getByRole("button", { name: "Export the others" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("library_export_catalog",
    expect.objectContaining({ leaveOutOwnedElsewhere: true })));
  expect(onClose).toHaveBeenCalled();
  cleanup();
  vi.mocked(invoke).mockClear();
  backend(owned, 1);
  render(<CatalogExportDialog directory="/Main" name="Main" selected={["/Other/Capture/t.arw"]} shown={[]}
    onClose={vi.fn()} onOpen={vi.fn()} />);
  await user.click(screen.getByRole("button", { name: "Destination parent folder" }));
  await user.click(screen.getByRole("button", { name: "Export 1 photo" }));
  expect((await screen.findByRole("alert")).textContent).toMatch(/belong to Wedding/);
  expect(invoke).not.toHaveBeenCalledWith("library_export_catalog", expect.anything());
});
