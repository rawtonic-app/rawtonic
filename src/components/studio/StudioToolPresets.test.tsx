import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ToolGroup } from "./StudioDevelop";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { loadPresetLibrary, resetPresetLibrary, usePresetStore, type UserPreset } from "../../hooks/usePresets";
import { debouncedSave, debouncedSetHistory } from "../../hooks/useEditorActions";
import { resetBuiltinPresets } from "../../utils/builtinPresets";
import { INITIAL_ADJUSTMENTS, type Adjustments } from "../../utils/adjustments";
import type { AppSettings, Preset } from "../ui/AppProperties";
import type { ToolParams } from "./StudioToolCopy";
import { groupPresets } from "./StudioPresets";
import { presetFitsTool, presetPreview, toolPresetToSave } from "./toolPresets";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mockInvoke = vi.mocked(invoke);

const GRAIN: ToolParams = [
  ["grainAmount", "Amount"],
  ["grainSize", "Size"],
  ["grainRoughness", "Roughness"],
  ["grainStructure", "Structure"],
];
const builtins: Omit<Preset, "builtin">[] = [
  {
    id: "grain-fine",
    version: 1,
    name: "Fine",
    presetType: "tool",
    adjustments: { grainAmount: 18, grainSize: 12, grainRoughness: 30, grainStructure: "even", grainVersion: 2 },
  },
  { id: "vignette-subtle", version: 1, name: "Subtle", presetType: "tool", adjustments: { vignetteAmount: -15 } },
  { id: "bw-soft", version: 1, name: "B&W soft", presetType: "style", adjustments: { blackAndWhite: true, contrast: -18 } },
  { id: "grain-style", version: 1, name: "Grain look", presetType: "style", adjustments: { grainAmount: 30, grainVersion: 2 } },
];
let stored: UserPreset[] | null;

beforeEach(() => {
  stored = [
    { preset: { id: "mine", name: "My grain", presetType: "tool", adjustments: { grainAmount: 50, grainVersion: 2 } } },
    { preset: { id: "look", name: "Mixed look", presetType: "tool", adjustments: { grainAmount: 10, contrast: 20 } } },
  ];
  resetBuiltinPresets();
  resetPresetLibrary();
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command) => {
    // No presets.json yet: load_presets fails or returns nothing.
    if (command === "load_presets") {
      if (stored === null) throw new Error("presets.json: No such file");
      return stored;
    }
    if (command === "load_builtin_presets") return builtins;
    if (command === "load_metadata") return {};
    return undefined;
  });
  const adjustments = { ...INITIAL_ADJUSTMENTS, grainAmount: 40, grainSize: 33, grainVersion: 2 } as Adjustments;
  useEditorStore.setState({
    selectedImage: { path: "/job/a.arw", isReady: true, isRaw: true } as never,
    adjustments,
    history: [adjustments],
    historyIndex: 0,
    previewOverride: null,
  });
  useLibraryStore.setState({ multiSelectedPaths: ["/job/a.arw"] });
  useSettingsStore.setState({ appSettings: {} as AppSettings });
});
afterEach(() => {
  cleanup();
  debouncedSave.cancel();
  debouncedSetHistory.cancel();
});

const renderTool = async (title = "Grain", copy: ToolParams | null = GRAIN) => {
  render(
    <div className="studio-workspace">
      <ToolGroup title={title} label={title} copy={copy ?? undefined}>
        <span />
      </ToolGroup>
    </div>,
  );
  await act(async () => {});
};
const openMenu = async (tool = "Grain") => {
  fireEvent.click(screen.getByRole("button", { name: `${tool} presets` }));
  await act(async () => {});
  return screen.getByRole("dialog", { name: `${tool} presets` });
};

describe("Tool preset menu", () => {
  it("reclamps an open menu when the viewport shrinks", async () => {
    const rect = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      left: 700, bottom: 500, width: 240, height: 200,
    } as DOMRect);
    const width = window.innerWidth;
    const height = window.innerHeight;
    try {
      await renderTool();
      const menu = await openMenu();
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 400 });
      Object.defineProperty(window, "innerHeight", { configurable: true, value: 300 });
      fireEvent(window, new Event("resize"));
      expect(menu.style.left).toBe("152px");
      expect(menu.style.top).toBe("92px");
    } finally {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: width });
      Object.defineProperty(window, "innerHeight", { configurable: true, value: height });
      rect.mockRestore();
    }
  });

  it("shows only on tools that opted in by their id, and only with parameters", async () => {
    await renderTool("Grain");
    expect(screen.getByRole("button", { name: "Grain presets" })).toBeTruthy();
    cleanup();
    await renderTool("Exposure", [["exposure", "Exposure"]]);
    expect(screen.queryByRole("button", { name: "Exposure presets" })).toBeNull();
    cleanup();
    // On a layer the tool has no copy parameters, hence no menu.
    await renderTool("Grain", null);
    expect(screen.queryByRole("button", { name: "Grain presets" })).toBeNull();
  });

  it("lists the tool's built-ins and the user's presets of this tool only", async () => {
    await renderTool();
    const menu = await openMenu();
    const builtin = within(menu).getByRole("group", { name: "Built-in" });
    expect(within(builtin).getAllByRole("button").map((b) => b.textContent)).toEqual(["Fine"]);
    const mine = within(menu).getByRole("group", { name: "My presets" });
    expect(within(mine).getByRole("button", { name: "My grain" })).toBeTruthy();
    // A preset that also sets another tool's settings is not this tool's.
    expect(within(menu).queryByRole("button", { name: "Mixed look" })).toBeNull();
  });

  it("keeps the built-ins when there is no presets file", async () => {
    stored = null;
    await renderTool();
    const menu = await openMenu();
    expect(within(menu).getByRole("button", { name: "Fine" })).toBeTruthy();
    expect(within(menu).getByText("None saved yet")).toBeTruthy();
  });

  it("hides the built-ins when Preferences turn them off", async () => {
    useSettingsStore.setState({ appSettings: { showBuiltinPresets: false } as AppSettings });
    await renderTool();
    const menu = await openMenu();
    expect(within(menu).queryByRole("group", { name: "Built-in" })).toBeNull();
  });

  it("previews on hover through the renderer and applies a click as one history step", async () => {
    await renderTool();
    const menu = await openMenu();
    const fine = within(menu).getByRole("button", { name: "Fine" });
    fireEvent.mouseEnter(fine);
    expect(useEditorStore.getState().previewOverride).toMatchObject({ grainAmount: 18, grainStructure: "even" });
    expect(useEditorStore.getState().adjustments.grainAmount).toBe(40);
    fireEvent.mouseLeave(fine);
    expect(useEditorStore.getState().previewOverride).toBeNull();

    fireEvent.mouseEnter(fine);
    fireEvent.click(fine);
    await act(async () => debouncedSetHistory.flush());
    const { adjustments, history, historyIndex, previewOverride } = useEditorStore.getState();
    expect(previewOverride).toBeNull();
    expect(adjustments).toMatchObject({ grainAmount: 18, grainSize: 12, grainRoughness: 30, grainStructure: "even", grainVersion: 2 });
    expect(history).toHaveLength(2);
    expect(historyIndex).toBe(1);
    // The menu closes after applying.
    expect(screen.queryByRole("dialog", { name: "Grain presets" })).toBeNull();
  });

  it("saves the tool's current settings as a preset in the tool's folder", async () => {
    await renderTool();
    const menu = await openMenu();
    fireEvent.click(within(menu).getByRole("button", { name: "Save current as preset…" }));
    fireEvent.change(within(menu).getByRole("textbox"), { target: { value: "Thirty-three" } });
    fireEvent.submit(within(menu).getByRole("form", { name: "Save current as preset…" }));
    const folder = usePresetStore.getState().presets.find((item) => item.folder?.name === "Grain")?.folder;
    expect(folder?.children).toHaveLength(1);
    const saved = folder!.children[0];
    expect(saved).toMatchObject({ name: "Thirty-three", presetType: "tool" });
    expect(folder).toHaveProperty("automatic", true);
    // Only this tool's settings, with the version they render by.
    expect(saved.adjustments).toEqual({
      grainAmount: 40,
      grainSize: 33,
      grainRoughness: INITIAL_ADJUSTMENTS.grainRoughness,
      grainStructure: undefined,
      grainVersion: 2,
    });
    expect(within(menu).getByRole("button", { name: "Thirty-three" })).toBeTruthy();
    // Written to the presets file when the menu closes.
    fireEvent.keyDown(window, { key: "Escape" });
    await act(async () => {});
    const save = mockInvoke.mock.calls.find(([command]) => command === "save_presets");
    expect(JSON.stringify(save?.[1])).toContain("Thirty-three");
  });

  it("deletes one of the user's presets after asking", async () => {
    await renderTool();
    const menu = await openMenu();
    fireEvent.click(within(menu).getByRole("button", { name: "Delete preset “My grain”" }));
    const confirm = within(menu).getByRole("alertdialog");
    fireEvent.click(within(confirm).getByRole("button", { name: "Delete" }));
    expect(within(menu).queryByRole("button", { name: "My grain" })).toBeNull();
    expect(usePresetStore.getState().presets.some((item) => item.preset?.id === "mine")).toBe(false);
    // Built-ins have no delete.
    expect(within(menu).queryByRole("button", { name: /Delete preset “Fine”/ })).toBeNull();
  });

  it("hides an automatic folder after its last tool preset is deleted, preserving an empty user folder", async () => {
    stored = [
      { folder: { id: "auto", name: "Grain", automatic: true, children: [stored![0].preset!] } },
      { folder: { id: "user", name: "My folder", children: [] } },
    ];
    await renderTool();
    const menu = await openMenu();
    fireEvent.click(within(menu).getByRole("button", { name: "Delete preset “My grain”" }));
    fireEvent.click(within(menu).getByRole("button", { name: "Delete" }));
    expect(groupPresets(usePresetStore.getState().presets).map((g) => g.folder?.id)).toEqual(["user"]);
  });

  it("keeps changes of the shared preset list not written yet when it opens", async () => {
    // Read once; then another user of the list (Styles & presets) changed it
    // and its write is pending.
    await act(async () => loadPresetLibrary());
    usePresetStore.getState().setPresets([
      { preset: { id: "fresh", name: "Not on disk yet", presetType: "tool", adjustments: { grainAmount: 5 } } },
    ]);
    mockInvoke.mockClear();
    await renderTool();
    const menu = await openMenu();
    expect(mockInvoke.mock.calls.some(([command]) => command === "load_presets")).toBe(false);
    expect(within(menu).getByRole("button", { name: "Not on disk yet" })).toBeTruthy();
    fireEvent.click(within(menu).getByRole("button", { name: "Save current as preset…" }));
    fireEvent.change(within(menu).getByRole("textbox"), { target: { value: "Second" } });
    fireEvent.submit(within(menu).getByRole("form", { name: "Save current as preset…" }));
    fireEvent.keyDown(window, { key: "Escape" });
    await act(async () => {});
    const saves = mockInvoke.mock.calls.filter(([command]) => command === "save_presets");
    const written = JSON.stringify(saves.at(-1)?.[1]);
    expect(written).toContain("Not on disk yet");
    expect(written).toContain("Second");
  });

  it("previews only presets the open source can show; the others still apply", async () => {
    const denoise = { id: "noise-ai", version: 1, name: "AI clean", presetType: "tool" as const, adjustments: { aiDenoise: 60, aiDenoiseVersion: 3 } };
    mockInvoke.mockImplementation(async (command) => {
      if (command === "load_presets") return [];
      if (command === "load_builtin_presets") return [denoise];
      if (command === "load_metadata") return {};
      return undefined;
    });
    await renderTool("Noise reduction", [["aiDenoise", "AI Denoise"]]);
    const menu = await openMenu("Noise reduction");
    const item = within(menu).getByRole("button", { name: "AI clean" });
    // AI Denoise develops the RAW again: the override would show the old source.
    fireEvent.mouseEnter(item);
    expect(useEditorStore.getState().previewOverride).toBeNull();
    fireEvent.click(item);
    expect(useEditorStore.getState().adjustments.aiDenoise).toBe(60);
  });
});

describe("reading presets.json", () => {
  it("is one read for every user, and a preset saved meanwhile survives it", async () => {
    let answer: (value: UserPreset[]) => void = () => {};
    mockInvoke.mockImplementation((command) => {
      if (command === "load_presets") return new Promise((resolve) => (answer = resolve as typeof answer));
      if (command === "load_builtin_presets") return Promise.resolve(builtins);
      return Promise.resolve(undefined);
    });
    void loadPresetLibrary();
    void loadPresetLibrary();
    expect(mockInvoke.mock.calls.filter(([command]) => command === "load_presets")).toHaveLength(1);
    // A tool preset saved before the file has been read.
    usePresetStore.getState().setPresets((list) => [
      ...list,
      { preset: { id: "new", name: "Saved early", presetType: "tool", adjustments: { grainAmount: 9 } } },
    ]);
    await act(async () => answer(stored!));
    const names = usePresetStore.getState().presets.map((item) => item.preset?.name);
    expect(names).toEqual(["My grain", "Mixed look", "Saved early"]);
    // And the merged list is written (after the 500 ms the writes wait).
    await act(async () => new Promise((resolve) => setTimeout(resolve, 600)));
    const written = mockInvoke.mock.calls.filter(([command]) => command === "save_presets").at(-1)?.[1];
    expect(JSON.stringify(written)).toContain("My grain");
    expect(JSON.stringify(written)).toContain("Saved early");
  });
});

describe("a failed read of presets.json", () => {
  it("writes nothing over the file and is tried again on the next use", async () => {
    let fail = true;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "load_presets") {
        if (fail) throw new Error("presets.json: busy");
        return stored;
      }
      if (command === "load_builtin_presets") return builtins;
      return undefined;
    });
    await act(async () => loadPresetLibrary());
    usePresetStore.getState().setPresets((list) => [
      ...list,
      { preset: { id: "new", name: "Saved early", presetType: "tool", adjustments: { grainAmount: 9 } } },
    ]);
    await act(async () => new Promise((resolve) => setTimeout(resolve, 600)));
    expect(mockInvoke.mock.calls.some(([command]) => command === "save_presets")).toBe(false);
    fail = false;
    await act(async () => loadPresetLibrary());
    expect(usePresetStore.getState().presets.map((item) => item.preset?.name)).toEqual(["My grain", "Mixed look", "Saved early"]);
    await act(async () => new Promise((resolve) => setTimeout(resolve, 600)));
    const written = JSON.stringify(mockInvoke.mock.calls.filter(([command]) => command === "save_presets").at(-1)?.[1]);
    expect(written).toContain("My grain");
    expect(written).toContain("Saved early");
  });
});

describe("which presets are a tool's", () => {
  const grain = GRAIN.map(([key]) => key as string);
  const preset = (adjustments: Partial<Adjustments>): Preset => ({ id: "p", name: "P", presetType: "tool", adjustments });
  it("takes the versions that go with the tool's parameters, no others", () => {
    expect(presetFitsTool(preset({ grainAmount: 25, grainVersion: 2 }), grain)).toBe(true);
    // A grain edit made with the earlier colour noise filter: applying it
    // would switch the photo's colour noise reduction too.
    expect(presetFitsTool(preset({ grainAmount: 25, grainVersion: 2, colorNoiseVersion: 1 }), grain)).toBe(false);
    expect(presetFitsTool(preset({ grainVersion: 2 }), grain)).toBe(false);
    const noise = ["colorNoiseReduction", "lumaNoiseReduction"];
    expect(presetFitsTool(preset({ colorNoiseReduction: 20, colorNoiseVersion: 10, lumaNoiseVersion: 2 }), noise)).toBe(true);
  });
});

describe("saving a tool preset", () => {
  it("states the colour noise filter of an edit from before its version, through JSON", () => {
    const legacy = { ...INITIAL_ADJUSTMENTS, colorNoiseReduction: 30, colorNoiseVersion: undefined } as Adjustments;
    const saved = JSON.parse(JSON.stringify(toolPresetToSave(legacy, ["colorNoiseReduction"])));
    expect(saved).toEqual({ colorNoiseReduction: 30, colorNoiseVersion: 1 });
    const current = { ...INITIAL_ADJUSTMENTS, colorNoiseReduction: 30, colorNoiseVersion: 10 } as Adjustments;
    expect(toolPresetToSave(current, ["colorNoiseReduction"]).colorNoiseVersion).toBe(10);
  });
});

describe("AI preset previews", () => {
  it("does not hover-preview AI preset masks with another photograph bitmap", () => {
    const masks = [{ subMasks: [{ type: "ai-sky", parameters: { maskDataBase64: "data:source" } }] }] as never;
    expect(presetPreview(INITIAL_ADJUSTMENTS, { masks }, true)).toBeNull();
  });

  it("previews a style's groups joined to the photo's own, as applying it does", () => {
    const base = { ...INITIAL_ADJUSTMENTS, hsl: { ...INITIAL_ADJUSTMENTS.hsl, blues: { hue: 22, saturation: 0, luminance: 0 } } };
    const previewed = presetPreview(base, { hsl: { reds: { hue: 5 } } } as never, true, { base, style: true });
    expect(previewed?.hsl).toMatchObject({ reds: { hue: 5 }, blues: { hue: 22 } });
  });
});
