import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "react-toastify";
import StudioPresets, { groupPresets } from "./StudioPresets";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { ContextMenuProvider } from "../../context/ContextMenuContext";
import { INITIAL_ADJUSTMENTS, initialAdjustmentsFor } from "../../utils/adjustments";
import type { UserPreset } from "../../hooks/usePresets";
import i18n from "i18next";
import { resetBuiltinPresets } from "../../utils/builtinPresets";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { AppSettings } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const openDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => openDialog(...args),
  save: (...args: unknown[]) => openDialog(...args),
}));
const apply = vi.fn();
const setAdjustments = vi.fn();
vi.mock("../../hooks/useEditorActions", () => ({
  useEditorActions: () => ({ applyAdjustmentsToPaths: apply, setAdjustments }),
}));
const mockInvoke = vi.mocked(invoke);

const stored: UserPreset[] = [
  { preset: { id: "warm", name: "Warm evening", adjustments: { temperature: 12 }, presetType: "tool" } },
  {
    folder: {
      id: "bw",
      name: "Black & white",
      children: [{ id: "hc", name: "High contrast", adjustments: { contrast: 40, saturation: -100 } }],
    },
  },
];

const builtins = [
  { id: "grain-fine", version: 1, name: "Fine", presetType: "tool", adjustments: { grainAmount: 18, grainVersion: 2 } },
  { id: "bw-high-contrast", version: 1, name: "B&W high contrast", presetType: "style", adjustments: { blackAndWhite: true, contrast: 35 } },
];
let builtinsFail = false;

beforeEach(() => {
  resetBuiltinPresets();
  builtinsFail = false;
  useSettingsStore.setState({ appSettings: {} as AppSettings });
  apply.mockReset();
  setAdjustments.mockReset();
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command) => {
    if (command === "load_presets") return stored;
    if (command === "load_builtin_presets") {
      if (builtinsFail) throw new Error("missing");
      return builtins;
    }
    return undefined;
  });
  useEditorStore.setState({
    selectedImage: { path: "/job/a.arw", isReady: true } as never,
    adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 0.5 },
  });
  useLibraryStore.setState({ multiSelectedPaths: ["/job/a.arw"] });
});
afterEach(cleanup);

const renderTool = async () => {
  render(
    <ContextMenuProvider>
      <StudioPresets />
    </ContextMenuProvider>,
  );
  await act(async () => {});
};

describe("groupPresets", () => {
  it("puts root presets first and filters by name", () => {
    expect(groupPresets(stored).map((g) => [g.folder?.name ?? null, g.presets.map((p) => p.id)])).toEqual([
      [null, ["warm"]],
      ["Black & white", ["hc"]],
    ]);
    expect(groupPresets(stored, "contrast").map((g) => g.presets.map((p) => p.id))).toEqual([["hc"]]);
  });
});

describe("Styles & presets tool", () => {
  it("applies a preset to the open photo, or to every selected photo", async () => {
    await renderTool();
    fireEvent.click(screen.getByText("Warm evening"));
    expect(apply).toHaveBeenLastCalledWith({ temperature: 12 }, ["/job/a.arw"]);
    useLibraryStore.setState({ multiSelectedPaths: ["/job/a.arw", "/job/b.arw"] });
    fireEvent.click(screen.getByText("High contrast"));
    expect(apply).toHaveBeenLastCalledWith({ contrast: 40, saturation: -100 }, ["/job/a.arw", "/job/b.arw"]);
  });

  it("does nothing while the photo is still loading", async () => {
    useEditorStore.setState({ selectedImage: { path: "/job/a.arw", isReady: false } as never });
    await renderTool();
    fireEvent.click(screen.getByText("Warm evening"));
    expect(apply).not.toHaveBeenCalled();
  });

  it("saves the changed settings of the open photo as a new preset", async () => {
    vi.useFakeTimers();
    try {
      await renderTool();
      fireEvent.click(screen.getByLabelText("New preset"));
      fireEvent.change(screen.getByLabelText("Preset name"), { target: { value: "Bright" } });
      fireEvent.click(screen.getByText("Save"));
      expect(screen.getByText("Bright")).toBeTruthy();
      await act(async () => {
        vi.advanceTimersByTime(600);
      });
      const saved = mockInvoke.mock.calls.find(([command]) => command === "save_presets")?.[1] as {
        presets: UserPreset[];
      };
      const bright = saved.presets.find((item) => item.preset?.name === "Bright")?.preset;
      expect(bright?.presetType).toBe("tool");
      expect(bright?.adjustments).toEqual({ exposure: 0.5 });
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps only real changes of a RAW whose recipe came back with sorted keys", async () => {
    vi.useFakeTimers();
    try {
      const raw = initialAdjustmentsFor(true);
      // A stored recipe: same values, keys in another order.
      const sorted = Object.fromEntries(Object.entries(raw.hsl).reverse()) as typeof raw.hsl;
      useEditorStore.setState({
        selectedImage: { path: "/job/a.arw", isReady: true, isRaw: true } as never,
        adjustments: { ...raw, hsl: sorted, contrast: 15 },
      });
      await renderTool();
      fireEvent.click(screen.getByLabelText("New preset"));
      fireEvent.change(screen.getByLabelText("Preset name"), { target: { value: "Punch" } });
      fireEvent.click(screen.getByText("Save"));
      await act(async () => {
        vi.advanceTimersByTime(600);
      });
      const saved = mockInvoke.mock.calls.find(([command]) => command === "save_presets")?.[1] as {
        presets: UserPreset[];
      };
      expect(saved.presets.find((item) => item.preset?.name === "Punch")?.preset?.adjustments).toEqual({
        contrast: 15,
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("asks before deleting", async () => {
    await renderTool();
    fireEvent.contextMenu(screen.getByText("Warm evening"));
    fireEvent.click(await screen.findByText("Delete…"));
    expect(screen.getByRole("alertdialog").textContent).toContain("Warm evening");
    fireEvent.click(screen.getByText("Cancel"));
    expect(screen.getByText("Warm evening")).toBeTruthy();
    fireEvent.contextMenu(screen.getByText("Warm evening"));
    fireEvent.click(await screen.findByText("Delete…"));
    fireEvent.click(screen.getByText("Delete"));
    expect(screen.queryByText("Warm evening")).toBeNull();
  });
});

describe("importing presets and styles", () => {
  it("imports Lightroom presets and Capture One styles chosen in the dialog", async () => {
    openDialog.mockResolvedValue(["/styles/Warm matte.costyle", "/lr/Film.xmp"]);
    mockInvoke.mockImplementation(async (command) =>
      command === "load_presets"
        ? stored
        : command === "handle_import_presets_from_files"
          ? { presets: stored, failures: [] }
          : undefined,
    );
    await renderTool();
    fireEvent.click(screen.getByRole("button", { name: "Import presets" }));
    await act(async () => {});
    expect(openDialog.mock.calls[0][0].filters[0].extensions).toContain("costyle");
    expect(mockInvoke).toHaveBeenCalledWith("handle_import_presets_from_files", {
      filePaths: ["/styles/Warm matte.costyle", "/lr/Film.xmp"],
    });
  });

  it("names the settings an import did not translate", async () => {
    const info = vi.spyOn(toast, "info").mockImplementation(() => "");
    openDialog.mockResolvedValue(["/lr/Film.xmp", "/styles/Warm matte.costyle"]);
    mockInvoke.mockImplementation(async (command) =>
      command === "load_presets"
        ? stored
        : command === "handle_import_presets_from_files"
          ? {
              presets: stored,
              failures: [],
              untranslated: [
                { fileName: "Film.xmp", settings: ["profile", "crop"], profile: "Vivid Blue" },
                { fileName: "Warm matte.costyle", settings: ["filmCurve"] },
              ],
            }
          : undefined,
    );
    await renderTool();
    fireEvent.click(screen.getByRole("button", { name: "Import presets" }));
    await act(async () => {});
    expect(info).toHaveBeenCalledWith(
      "Not translated — Film.xmp: profile “Vivid Blue”, crop · Warm matte.costyle: film curve",
      { autoClose: 12000 },
    );
    info.mockRestore();
  });

  it("does nothing when the dialog is cancelled", async () => {
    openDialog.mockResolvedValue(null);
    await renderTool();
    fireEvent.click(screen.getByRole("button", { name: "Import presets" }));
    await act(async () => {});
    expect(mockInvoke).not.toHaveBeenCalledWith("handle_import_presets_from_files", expect.anything());
  });
});

describe("preset amount", () => {
  it("blends the last preset applied to the open photo with its settings before", async () => {
    await renderTool();
    fireEvent.click(screen.getByRole("button", { name: /High contrast/ }));
    expect(apply).toHaveBeenCalledWith({ contrast: 40, saturation: -100 }, ["/job/a.arw"]);
    const slider = screen.getByLabelText("Preset amount") as HTMLInputElement;
    expect(slider.value).toBe("100");
    fireEvent.change(slider, { target: { value: "50" } });
    const update = setAdjustments.mock.lastCall![0] as (current: Record<string, unknown>) => Record<string, unknown>;
    const next = update({ ...INITIAL_ADJUSTMENTS, exposure: 0.5, contrast: 40, saturation: -100 });
    expect(next).toMatchObject({ contrast: 20, saturation: -50, exposure: 0.5 });
  });

  it("offers no amount when a preset goes to several photographs", async () => {
    useLibraryStore.setState({ multiSelectedPaths: ["/job/a.arw", "/job/b.arw"] });
    await renderTool();
    fireEvent.click(screen.getByRole("button", { name: /Warm evening/ }));
    expect(apply).toHaveBeenCalledWith({ temperature: 12 }, ["/job/a.arw", "/job/b.arw"]);
    expect(screen.queryByLabelText("Preset amount")).toBeNull();
  });
});

describe("preview and folders (Lightroom, Capture One)", () => {
  it("previews a preset on the open photo while hovered, without applying it", async () => {
    vi.useFakeTimers();
    await renderTool();
    fireEvent.mouseEnter(screen.getByText("Warm evening").closest("button")!);
    act(() => vi.advanceTimersByTime(200));
    expect(useEditorStore.getState().previewOverride).toMatchObject({ temperature: 12, exposure: 0.5 });
    expect(apply).not.toHaveBeenCalled();
    fireEvent.mouseLeave(screen.getByText("Warm evening").closest("button")!);
    expect(useEditorStore.getState().previewOverride).toBeNull();
    vi.useRealTimers();
  });

  it("makes a folder and moves a preset into it", async () => {
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        disconnect() {}
      },
    );
    await renderTool();
    fireEvent.click(screen.getByRole("button", { name: "New preset folder" }));
    const name = screen.getByRole("textbox", { name: "Folder name" });
    fireEvent.change(name, { target: { value: "Portraits" } });
    fireEvent.keyDown(name, { key: "Enter" });
    await act(async () => {});
    const folder = screen.getByRole("button", { name: /Portraits/ });
    expect(folder.textContent).toMatch(/0$/);
    fireEvent.contextMenu(screen.getByText("Warm evening"));
    fireEvent.mouseEnter(screen.getAllByRole("menuitem", { name: /Move to/ }).at(-1)!.parentElement!);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 200));
    });
    fireEvent.click(screen.getAllByRole("menuitem", { name: "Portraits" }).at(-1)!);
    await act(async () => {});
    expect(screen.getByRole("button", { name: /Portraits/ }).textContent).toMatch(/1$/);
    vi.unstubAllGlobals();
  });

  it("deletes a folder after asking, naming how many presets go with it", async () => {
    await renderTool();
    fireEvent.contextMenu(screen.getByText("Black & white"));
    fireEvent.click(screen.getAllByRole("menuitem", { name: /Delete/ }).at(-1)!);
    expect(screen.getByRole("alertdialog", { name: "Delete folder" }).textContent).toMatch(/its 1 preset\?/);
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await act(async () => {});
    expect(screen.queryByText("Black & white")).toBeNull();
  });
});


it.each([["New preset", "Preset name"], ["New preset folder", "Folder name"]])("returns focus after Escape from %s", async (button, field) => {
  await renderTool();
  const trigger = screen.getByRole("button", { name: button });
  trigger.focus();
  fireEvent.click(trigger);
  const input = screen.getByRole("textbox", { name: field });
  expect(document.activeElement).toBe(input);
  fireEvent.keyDown(input, { key: "Escape" });
  expect(screen.queryByRole("textbox", { name: field })).toBeNull();
  expect(document.activeElement).toBe(trigger);
});

describe("built-in presets", () => {
  const builtinFolder = () => screen.getByRole("button", { name: /^Built-in/ });

  it("lists Rawtonic's own styles in a Built-in folder, translated; its tool presets are in the tools", async () => {
    await act(async () => {
      await i18n.changeLanguage("de");
    });
    try {
      await renderTool();
      expect(screen.getByRole("button", { name: /^Mitgeliefert/ })).toBeTruthy();
      expect(screen.getByText("S/W hoher Kontrast")).toBeTruthy();
      expect(screen.queryByText("Fein")).toBeNull();
    } finally {
      await act(async () => {
        await i18n.changeLanguage("en");
      });
    }
  });

  it("applies a built-in like any preset; built-ins have no menu to rename or delete them", async () => {
    await renderTool();
    expect(builtinFolder()).toBeTruthy();
    fireEvent.contextMenu(screen.getByText("B&W high contrast"));
    expect(screen.queryByText("Rename")).toBeNull();
    fireEvent.click(screen.getByText("B&W high contrast"));
    expect(apply).toHaveBeenLastCalledWith({ blackAndWhite: true, contrast: 35 }, ["/job/a.arw"]);
    fireEvent.contextMenu(builtinFolder());
    expect(screen.queryByText("Rename")).toBeNull();
  });

  it("keeps the built-ins without a presets file and the user's presets without built-ins", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "load_presets") throw new Error("presets.json: No such file");
      if (command === "load_builtin_presets") return builtins;
      return undefined;
    });
    await renderTool();
    expect(screen.getByText("B&W high contrast")).toBeTruthy();
    cleanup();
    resetBuiltinPresets();
    mockInvoke.mockImplementation(async (command) => {
      if (command === "load_presets") return stored;
      if (command === "load_builtin_presets") throw new Error("missing");
      return undefined;
    });
    await renderTool();
    expect(screen.getByText("Warm evening")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Built-in/ })).toBeNull();
  });

  it("hides the Built-in folder when Preferences turn it off", async () => {
    useSettingsStore.setState({ appSettings: { showBuiltinPresets: false } as AppSettings });
    await renderTool();
    expect(screen.queryByRole("button", { name: /^Built-in/ })).toBeNull();
    expect(screen.getByText("Warm evening")).toBeTruthy();
  });
});
