import "../../i18n";
import i18n from "i18next";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

const { invoke, open, toast } = vi.hoisted(() => ({
  invoke: vi.fn(),
  open: vi.fn(),
  toast: { warn: vi.fn(), info: vi.fn(), error: vi.fn() },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open }));
vi.mock("react-toastify", () => ({ toast }));

import StudioLut from "./StudioLut";
import { useEditorStore } from "../../store/useEditorStore";
import { debouncedSetHistory } from "../../hooks/useEditorActions";
import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";
import type { SelectedImage } from "../ui/AppProperties";

beforeEach(() => {
  invoke.mockReset();
  open.mockReset();
  Object.values(toast).forEach((f) => f.mockReset());
  invoke.mockImplementation((name: string) =>
    Promise.resolve(
      name === "list_luts"
        ? [
            { name: "Warm 200", path: "/r/Warm 200.cube", isBuiltIn: true },
            { name: "Mine", path: "/u/Mine.cube", isBuiltIn: false },
          ]
        : name === "load_and_parse_lut"
          ? { size: 33 }
          : undefined,
    ),
  );
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    selectedImage: { path: "/s/a.NEF", isRaw: true, isReady: true } as SelectedImage,
  });
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
});

it("applies a LUT with its amount and removes it again", async () => {
  await act(async () => {
    render(<StudioLut disabled={false} onInteraction={vi.fn()} />);
  });
  const select = screen.getByRole("combobox", { name: "LUT" });
  expect([...select.querySelectorAll("option")].map((o) => o.textContent)).toEqual(["None", "Warm", "Mine"]);
  await act(async () => {
    fireEvent.change(select, { target: { value: "/u/Mine.cube" } });
  });
  expect(invoke).toHaveBeenCalledWith("load_and_parse_lut", { path: "/u/Mine.cube" });
  expect(useEditorStore.getState().adjustments).toMatchObject({ lutPath: "/u/Mine.cube", lutSize: 33 });
  const amount = screen.getByRole("textbox", { name: "LUT amount value" });
  fireEvent.focus(amount);
  fireEvent.change(amount, { target: { value: "40" } });
  fireEvent.blur(amount);
  expect(useEditorStore.getState().adjustments.lutIntensity).toBe(40);
  // Up to 200 %, as a creative profile's Amount.
  fireEvent.focus(amount);
  fireEvent.change(amount, { target: { value: "250" } });
  fireEvent.blur(amount);
  expect(useEditorStore.getState().adjustments.lutIntensity).toBe(200);
  fireEvent.change(select, { target: { value: "" } });
  expect(useEditorStore.getState().adjustments).toMatchObject({ lutPath: null, lutIntensity: 100 });
});

it("removes an imported LUT, not a built-in one", async () => {
  useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, lutPath: "/r/Warm 200.cube" } }));
  await act(async () => {
    render(<StudioLut disabled={false} onInteraction={vi.fn()} />);
  });
  expect(screen.queryByRole("button", { name: "Remove" })).toBeNull();
  useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, lutPath: "/u/Mine.cube" } }));
  await act(async () => {
    fireEvent.click(await screen.findByRole("button", { name: "Remove" }));
  });
  expect(invoke).toHaveBeenCalledWith("remove_lut", { path: "/u/Mine.cube" });
  expect(useEditorStore.getState().adjustments.lutPath).toBeNull();
});

it("groups imported LUTs by pack and reports the files a folder import left out", async () => {
  const packs = [
    { name: "Warm 200", path: "/r/Warm 200.cube", isBuiltIn: true, group: null },
    { name: "Mine", path: "/u/Mine.cube", isBuiltIn: false, group: null },
    { name: "Dusk", path: "/u/Summer/Dusk.cube", isBuiltIn: false, group: "Summer" },
    { name: "Night", path: "/u/Summer/Extras/Night.cube", isBuiltIn: false, group: "Summer / Extras" },
    { name: "Bright", path: "/u/Autumn/Bright.cube", isBuiltIn: false, group: "Autumn" },
  ];
  invoke.mockImplementation((name: string) =>
    Promise.resolve(
      name === "list_luts"
        ? packs.slice(0, 2)
        : name === "import_luts"
          ? {
              luts: packs,
              imported: 3,
              skipped: [
                {
                  file: "Summer/Broken.cube",
                  reason: "LUT data size mismatch",
                  issue: { kind: "incomplete", expected: 24, found: 3 },
                },
              ],
            }
          : undefined,
    ),
  );
  open.mockResolvedValue(["/home/me/Summer"]);
  await act(async () => {
    render(<StudioLut disabled={false} onInteraction={vi.fn()} />);
  });
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Import folder…" }));
  });
  expect(open).toHaveBeenCalledWith({ directory: true, multiple: true });
  expect(invoke).toHaveBeenCalledWith("import_luts", { sourcePaths: ["/home/me/Summer"] });
  const select = screen.getByRole("combobox", { name: "LUT" });
  const groups = [...select.querySelectorAll("optgroup")].map((g) => [
    g.getAttribute("label"),
    [...g.querySelectorAll("option")].map((o) => o.textContent),
  ]);
  expect(groups).toEqual([
    ["Built in", ["Warm"]],
    ["Imported", ["Mine"]],
    ["Autumn", ["Bright"]],
    ["Summer", ["Dusk"]],
    ["Summer / Extras", ["Night"]],
  ]);
  expect(toast.warn).toHaveBeenCalledTimes(1);
  expect(toast.warn.mock.calls[0][0]).toContain(
    "Summer/Broken.cube: The table is incomplete or damaged (3 of 24 values).",
  );
  expect(toast.warn.mock.calls[0][0]).toContain("3 imported");
});

it("translates why files were not imported, with the system's error where it says more", async () => {
  const { default: i18n } = await import("i18next");
  invoke.mockImplementation((name: string) =>
    Promise.resolve(
      name === "import_luts"
        ? {
            luts: [],
            imported: 0,
            skipped: [
              { file: "P/a/b/c/d", reason: "Folders nested deeper", issue: { kind: "tooDeep", max: 4 } },
              { file: "P/Copy.cube", reason: "Permission denied", issue: { kind: "copyFailed" } },
              { file: "P/Bad.cube", reason: "Line 3: 'x' is not a number", issue: { kind: "badLine", line: 3 } },
              { file: "P/New.cube", reason: "A newer reason", issue: { kind: "notKnownYet" } },
            ],
          }
        : [],
    ),
  );
  open.mockResolvedValue(["/home/me/P"]);
  await i18n.changeLanguage("de");
  try {
    await act(async () => {
      render(<StudioLut disabled={false} onInteraction={vi.fn()} />);
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Ordner importieren…" }));
    });
    const text = toast.warn.mock.calls[0][0] as string;
    expect(text).toContain("P/a/b/c/d: Ordner, die tiefer als 4 Ebenen verschachtelt sind, werden nicht importiert.");
    expect(text).toContain("P/Copy.cube: Die Kopie konnte nicht gespeichert werden. (Permission denied)");
    expect(text).toContain("P/Bad.cube: Zeile 3 kann nicht gelesen werden.");
    expect(text).not.toContain("is not a number");
    // A kind this version has no text for keeps the English reason.
    expect(text).toContain("P/New.cube: A newer reason");
  } finally {
    await i18n.changeLanguage("en");
  }
});

it("translates all built-in names without changing their saved paths or imported names", async () => {
  const names = ["Portrait 400", "Cinema 250D", "Cinema 500T", "Pastel 400", "Vivid 100", "Warm 200"];
  invoke.mockResolvedValue([
    ...names.map((name) => ({ name, path: `/r/${name}.cube`, isBuiltIn: true })),
    { name: "Warm 200", path: "/u/Warm 200.cube", isBuiltIn: false },
  ]);
  for (const language of Object.keys(i18n.options.resources!)) {
    await act(async () => { await i18n.changeLanguage(language); });
    const view = await act(async () => render(<StudioLut disabled={false} onInteraction={vi.fn()} />));
    const options = [...screen.getByRole("combobox", { name: "LUT" }).querySelectorAll("option")];
    const labels = options.slice(1, 7).map((o) => o.textContent!);
    expect(labels.every((label) => !/\d|studio\.lut/.test(label))).toBe(true);
    expect(new Set(labels).size).toBe(6);
    expect(options.slice(1, 7).map((o) => o.value)).toEqual(names.map((name) => `/r/${name}.cube`));
    expect(options.at(-1)?.textContent).toBe("Warm 200");
    view.unmount();
  }
  await act(async () => { await i18n.changeLanguage("en"); });
});

it("stores a descriptive name with a built-in selection while preserving its identifier", async () => {
  await act(async () => { render(<StudioLut disabled={false} onInteraction={vi.fn()} />); });
  await act(async () => { fireEvent.change(screen.getByRole("combobox", { name: "LUT" }), { target: { value: "/r/Warm 200.cube" } }); });
  expect(useEditorStore.getState().adjustments).toMatchObject({ lutPath: "/r/Warm 200.cube", lutName: "Warm" });
});
