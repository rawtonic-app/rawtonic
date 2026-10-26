import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { invoke } from "@tauri-apps/api/core";
import XmpSyncDialog from "./XmpSyncDialog";
import type { XmpReviewEntry } from "../../utils/xmpReview";

vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => false, invoke: vi.fn() }));

const conflict = (name: string, offline = false): XmpReviewEntry => ({
  path: `/jobs/Acme/Capture/${name}`,
  state: "conflict",
  offline,
  volume: null,
  copies: [],
});
const empty = { done: [], offline: [], conflicts: [], failed: [], undo: [] };

afterEach(() => cleanup());
beforeEach(() => {
  vi.clearAllMocks();
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
  };
});

function open(syncEnabled = true) {
  const onEntries = vi.fn();
  render(<XmpSyncDialog directory="/jobs/Acme" syncEnabled={syncEnabled} onClose={vi.fn()} onEntries={onEntries} />);
  return { onEntries, dialog: screen.getByRole("dialog", { name: "XMP sidecars" }) };
}

describe("XMP review dialog", () => {
  it("reads every conflict from the sidecars and undoes it", async () => {
    const user = userEvent.setup();
    let entries = [conflict("a.nef"), conflict("b.nef"), conflict("c.nef", true)];
    const steps = [{ asset: "a" }, { asset: "b" }];
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "session_xmp_review") return entries;
      if (command === "session_xmp_resolve") {
        entries = [conflict("c.nef", true)];
        return { ...empty, done: ["/jobs/Acme/Capture/a.nef", "/jobs/Acme/Capture/b.nef"], offline: ["/jobs/Acme/Capture/c.nef"], undo: steps };
      }
      if (command === "session_xmp_undo") {
        entries = [conflict("a.nef"), conflict("b.nef"), conflict("c.nef", true)];
        return { ...empty, done: ["/jobs/Acme/Capture/a.nef", "/jobs/Acme/Capture/b.nef"] };
      }
      throw new Error(`unexpected ${command}`);
    });
    const { dialog, onEntries } = open();
    expect(await within(dialog).findByRole("heading", { name: "3 conflicts" })).toBeTruthy();
    // An offline photograph cannot be resolved one by one until it is back.
    expect((within(dialog).getByRole("button", { name: "Use XMP for c.nef" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(within(dialog).getByRole("button", { name: "Use XMP for all" }));
    expect(invoke).toHaveBeenCalledWith("session_xmp_resolve", {
      directory: "/jobs/Acme",
      paths: ["/jobs/Acme/Capture/a.nef", "/jobs/Acme/Capture/b.nef", "/jobs/Acme/Capture/c.nef"],
      keep: "xmp",
    });
    expect(await within(dialog).findByText("Done: 2 · offline: 1 · conflicts: 0 · failed: 0")).toBeTruthy();
    await waitFor(() => expect(onEntries).toHaveBeenLastCalledWith([conflict("c.nef", true)], false));
    await user.click(within(dialog).getByRole("button", { name: "Undo Use XMP (2)" }));
    expect(invoke).toHaveBeenCalledWith("session_xmp_undo", { directory: "/jobs/Acme", steps });
    expect(await within(dialog).findByRole("heading", { name: "3 conflicts" })).toBeTruthy();
    expect(within(dialog).queryByRole("button", { name: /Undo Use XMP/ })).toBeNull();
  });

  it("names the photographs a sync could not write and why", async () => {
    const user = userEvent.setup();
    let entries: XmpReviewEntry[] = [{ ...conflict("a.nef"), state: "pending" }];
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "session_xmp_review") return entries;
      if (command === "session_xmp_sync") {
        entries = [{ ...conflict("a.nef"), state: "pending" }];
        return { ...empty, failed: [{ path: "/jobs/Acme/Capture/a.nef", reason: "The XMP sidecar is read-only" }] };
      }
      throw new Error(`unexpected ${command}`);
    });
    const { dialog } = open();
    expect(await within(dialog).findByRole("heading", { name: "1 pending" })).toBeTruthy();
    await user.click(within(dialog).getByRole("button", { name: "Sync now" }));
    expect(await within(dialog).findByText("The XMP sidecar is read-only")).toBeTruthy();
    expect(within(dialog).getByText("a.nef")).toBeTruthy();
  });

  it("says what XMP Sync in Preferences covers when it is off", async () => {
    vi.mocked(invoke).mockResolvedValue([]);
    const { dialog } = open(false);
    expect(within(dialog).getByText(/XMP Sync is off in Preferences/)).toBeTruthy();
    expect(await within(dialog).findByText("All XMP sidecars are in sync.")).toBeTruthy();
  });
});

describe("XMP review dialog undo", () => {
  it("keeps the Use XMP undo when Sync now retries a failed write-back", async () => {
    const user = userEvent.setup();
    let entries: XmpReviewEntry[] = [conflict("a.nef")];
    const steps = [{ asset: "a" }];
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "session_xmp_review") return entries;
      if (command === "session_xmp_resolve") {
        entries = [{ ...conflict("a.nef"), state: "pending" }];
        return { ...empty, failed: [{ path: "/jobs/Acme/Capture/a.nef", reason: "Read from the XMP sidecar; writing it back failed: The XMP sidecar is read-only" }], undo: steps };
      }
      if (command === "session_xmp_sync") {
        entries = [];
        return { ...empty, done: ["/jobs/Acme/Capture/a.nef"] };
      }
      if (command === "session_xmp_undo") return { ...empty, done: ["/jobs/Acme/Capture/a.nef"] };
      throw new Error(`unexpected ${command}`);
    });
    const { dialog } = open();
    await user.click(await within(dialog).findByRole("button", { name: "Use XMP for a.nef" }));
    expect(await within(dialog).findByText(/writing it back failed/)).toBeTruthy();
    await user.click(await within(dialog).findByRole("button", { name: "Sync now" }));
    expect(await within(dialog).findByText("All XMP sidecars are in sync.")).toBeTruthy();
    await user.click(within(dialog).getByRole("button", { name: "Undo Use XMP" }));
    expect(invoke).toHaveBeenCalledWith("session_xmp_undo", { directory: "/jobs/Acme", steps });
    await waitFor(() => expect(within(dialog).queryByRole("button", { name: /Undo Use XMP/ })).toBeNull());
  });

  it("keeps the steps an undo could not take back", async () => {
    const user = userEvent.setup();
    const steps = [{ asset: "a" }];
    let entries: XmpReviewEntry[] = [conflict("a.nef")];
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "session_xmp_review") return entries;
      if (command === "session_xmp_resolve") {
        entries = [];
        return { ...empty, done: ["/jobs/Acme/Capture/a.nef"], undo: steps };
      }
      if (command === "session_xmp_undo")
        return { ...empty, failed: [{ path: "/jobs/Acme/Capture/a.nef", reason: "database is locked" }], undo: steps };
      throw new Error(`unexpected ${command}`);
    });
    const { dialog } = open();
    await user.click(await within(dialog).findByRole("button", { name: "Use XMP for a.nef" }));
    await user.click(await within(dialog).findByRole("button", { name: "Undo Use XMP" }));
    expect(await within(dialog).findByText("database is locked")).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: "Undo Use XMP" })).toBeTruthy();
  });
});
