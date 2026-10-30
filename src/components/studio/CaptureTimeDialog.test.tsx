import "../../i18n";
import i18n from "i18next";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { CaptureTimeDialogHost, useCaptureTimeDialog } from "./CaptureTimeDialog";
import { canUndo, undo, useCatalogUndoStore } from "../../utils/appUndo";
import type { CaptureTime, PlannedChange, PlanRequest } from "../../utils/captureTime";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const LIBRARY = "/jobs/Wedding";
const a: CaptureTime = { local: "2026-10-30 12:00:00", subsec: "05", offset: "+02:00" };
const b: CaptureTime = { local: "2026-10-30 12:30:00", subsec: null, offset: null };
const later = (time: CaptureTime): CaptureTime => ({ ...time, local: time.local.replace(" 12:", " 13:") });

let applies: { library: string; changes: PlannedChange[] }[] = [];
let plans: PlanRequest[] = [];
let conflictNext = false;
let library: string | null = LIBRARY;

beforeEach(async () => {
  applies = [];
  plans = [];
  conflictNext = false;
  library = LIBRARY;
  useCatalogUndoStore.setState({ past: [], future: [] });
  await i18n.changeLanguage("en");
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === "capture_time_preview")
      return library
        ? {
            library,
            items: [
              { assetId: "A", path: "/jobs/Wedding/Capture/A.CR3", camera: "Canon EOS R5", source: a, corrected: null },
              { assetId: "B", path: "/jobs/Wedding/Capture/B.ARW?vc=1", camera: "Sony ILCE-7M4", source: b, corrected: null },
            ],
            excluded: [{ path: "/elsewhere/C.NEF", reason: "notInLibrary" }],
          }
        : { library: null, items: [], excluded: [{ path: "/elsewhere/C.NEF", reason: "notInLibrary" }] };
    if (name === "capture_time_plan") {
      const { request } = args as { request: PlanRequest };
      plans.push(request);
      if (request.kind === "shift" && !/^\d*$/.test(request.parts.hours))
        return { shift: null, error: { kind: "field", field: "hours" }, changes: [], outOfRange: [], beforeRange: null, afterRange: null };
      const shifted = request.kind === "shift" && request.parts.hours === "1";
      const changes = shifted
        ? [
            { assetId: "A", path: "/jobs/Wedding/Capture/A.CR3", expected: null, new: later(a), before: a, after: later(a) },
            { assetId: "B", path: "/jobs/Wedding/Capture/B.ARW?vc=1", expected: null, new: later(b), before: b, after: later(b) },
          ]
        : [];
      return {
        shift: { negative: false, days: "0", hours: shifted ? "1" : "0", minutes: "0", seconds: "0" },
        error: null,
        changes,
        outOfRange: [],
        beforeRange: shifted ? [a, b] : null,
        afterRange: shifted ? [later(a), later(b)] : null,
      };
    }
    if (name === "capture_time_apply") {
      const call = args as { library: string; changes: PlannedChange[] };
      applies.push({ library: call.library, changes: call.changes });
      if (conflictNext) return { conflicts: [call.changes[0].path], failed: [], undo: [] };
      return {
        conflicts: [],
        failed: [],
        undo: call.changes.map((c) => ({ ...c, expected: c.new, new: c.expected, before: c.after, after: c.before })),
      };
    }
    return undefined;
  });
});

afterEach(() => {
  act(() => useCaptureTimeDialog.getState().close());
  cleanup();
});

async function openDialog() {
  const opener = document.createElement("button");
  document.body.appendChild(opener);
  opener.focus();
  render(<CaptureTimeDialogHost />);
  act(() => useCaptureTimeDialog.getState().open(["/jobs/Wedding/Capture/A.CR3", "/jobs/Wedding/Capture/B.ARW?vc=1", "/elsewhere/C.NEF"]));
  await screen.findByText(/2 photos/);
  return opener;
}

it("previews, applies one shift to the selection as one undo step, and undoes it", async () => {
  await openDialog();
  expect(screen.getByText(/Canon EOS R5 \(1\), Sony ILCE-7M4 \(1\)/)).toBeTruthy();
  expect(screen.getByText("1 photo left out")).toBeTruthy();
  expect(screen.getByRole("button", { name: /Apply to/ })).toHaveProperty("disabled", true);
  fireEvent.change(screen.getByLabelText("Hours"), { target: { value: "1" } });
  const apply = await screen.findByRole("button", { name: "Apply to 2 photos" });
  expect(apply).toHaveProperty("disabled", false);
  expect(screen.getByText("A.CR3: 2026-10-30 12:00:00.05 +02:00 → 2026-10-30 13:00:00.05 +02:00")).toBeTruthy();
  // Enter in the form applies.
  fireEvent.submit(screen.getByRole("dialog"));
  await waitFor(() => expect(applies).toHaveLength(1));
  expect(applies[0].library).toBe(LIBRARY);
  expect(applies[0].changes.map((c) => c.after.local)).toEqual(["2026-10-30 13:00:00", "2026-10-30 13:30:00"]);
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(canUndo()).toBe(true);
  await act(() => undo());
  expect(applies).toHaveLength(2);
  expect(applies[1].changes.map((c) => [c.expected?.local, c.new, c.after.local])).toEqual([
    ["2026-10-30 13:00:00", null, "2026-10-30 12:00:00"],
    ["2026-10-30 13:30:00", null, "2026-10-30 12:30:00"],
  ]);
});

it("names a malformed part and keeps Apply off", async () => {
  await openDialog();
  fireEvent.change(screen.getByLabelText("Hours"), { target: { value: "1.5" } });
  const field = await screen.findByText(/Enter a whole number/);
  expect(screen.getByLabelText("Hours").getAttribute("aria-invalid")).toBe("true");
  expect(screen.getByLabelText("Hours").getAttribute("aria-describedby")).toBe(field.id);
  expect(screen.getByRole("button", { name: /Apply to/ })).toHaveProperty("disabled", true);
});

it("writes nothing on a conflict and reads the selection again", async () => {
  await openDialog();
  conflictNext = true;
  fireEvent.change(screen.getByLabelText("Hours"), { target: { value: "1" } });
  fireEvent.click(await screen.findByRole("button", { name: "Apply to 2 photos" }));
  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(screen.getByRole("dialog")).toBeTruthy();
  expect(canUndo()).toBe(false);
  await waitFor(() =>
    expect(vi.mocked(invoke).mock.calls.filter(([name]) => name === "capture_time_preview")).toHaveLength(2),
  );
});

it("asks to add unmanaged photos to a library first", async () => {
  library = null;
  render(<CaptureTimeDialogHost />);
  act(() => useCaptureTimeDialog.getState().open(["/elsewhere/C.NEF"]));
  expect(await screen.findByText(/Add these photos to one first/)).toBeTruthy();
  expect(screen.getByRole("button", { name: /Apply to/ })).toHaveProperty("disabled", true);
});

it("closes on Escape and gives the focus back", async () => {
  const opener = await openDialog();
  expect(document.activeElement).toBe(screen.getByRole("dialog"));
  fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(document.activeElement).toBe(opener);
  opener.remove();
});

it("is translated", async () => {
  await i18n.changeLanguage("de");
  render(<CaptureTimeDialogHost />);
  act(() => useCaptureTimeDialog.getState().open(["/jobs/Wedding/Capture/A.CR3"]));
  expect(await screen.findByRole("dialog", { name: "Aufnahmezeit verschieben" })).toBeTruthy();
  expect(screen.getByLabelText("Stunden")).toBeTruthy();
  await i18n.changeLanguage("en");
});
