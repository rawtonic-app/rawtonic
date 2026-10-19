import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useLibraryStore } from "../store/useLibraryStore";
import { useSettingsStore } from "../store/useSettingsStore";
import { isAnyModalOpen, useUIStore } from "../store/useUIStore";
import { AppSettings, Panel } from "../components/ui/AppProperties";
import { useEditorStore, debouncedSetHistory } from "../store/useEditorStore";
import { readModuleLayout } from "../utils/panelLayout";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve()),
}));
vi.mock("react-toastify", () => ({
  toast: { error: vi.fn(), success: vi.fn() },
}));

const props = {
  sortedImageList: [],
  handleBackToLibrary: vi.fn(),
  handleDeleteSelected: vi.fn(),
  handleUndoFileOperation: vi.fn(),
  handleRedoFileOperation: vi.fn(),
  handleImageSelect: vi.fn(),
  handlePasteFiles: vi.fn(),
  handleZoomChange: vi.fn(),
};
const press = (code: string, key = code) =>
  window.dispatchEvent(
    new KeyboardEvent("keydown", {
      code,
      key,
      bubbles: true,
      cancelable: true,
    }),
  );

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({
    appSettings: { keybinds: {} } as unknown as AppSettings,
    osPlatform: "macos",
  });
  useLibraryStore.setState({
    rootPaths: ["/session/Capture"],
    imageList: [],
    multiSelectedPaths: [],
  });
  useUIStore.setState({ activeView: "library", isSettingsOpen: false });
  renderHook(() => useKeyboardShortcuts(props));
});
afterEach(() => {
  cleanup();
  document.body.innerHTML = "";
});

it("keeps the session in view when Escape is pressed in the Library", () => {
  press("Escape");
  expect(useLibraryStore.getState().rootPaths).toEqual(["/session/Capture"]);
  expect(props.handleBackToLibrary).not.toHaveBeenCalled();
});

it("goes back to the Library on Escape in Develop", () => {
  useUIStore.setState({ activeView: "editor" });
  press("Escape");
  expect(props.handleBackToLibrary).toHaveBeenCalledOnce();
});

it("closes Preferences on Escape without leaving Develop", () => {
  useUIStore.setState({ activeView: "editor", isSettingsOpen: true });
  // The Preferences overlay, as StudioWorkspace renders it.
  document.body.innerHTML =
    '<div role="dialog" aria-modal="true" aria-label="Preferences"></div>';
  press("Escape");
  expect(useUIStore.getState().isSettingsOpen).toBe(false);
  expect(props.handleBackToLibrary).not.toHaveBeenCalled();
});

it("leaves Preferences open when the Escape was already handled (shortcut recording)", () => {
  useUIStore.setState({ isSettingsOpen: true });
  const consume = (event: KeyboardEvent) => event.preventDefault();
  window.addEventListener("keydown", consume, { capture: true });
  press("Escape");
  window.removeEventListener("keydown", consume, { capture: true });
  expect(useUIStore.getState().isSettingsOpen).toBe(true);
});

it.each([
  "mergeModalState",
  "cullingModalState",
] as const)("leaves keys to the open %s dialog", (modal) => {
  useUIStore.setState({
    activeView: "editor",
    [modal]: { ...useUIStore.getState()[modal], isOpen: true },
  });
  press("Escape");
  expect(props.handleBackToLibrary).not.toHaveBeenCalled();
  useUIStore.setState({
    [modal]: { ...useUIStore.getState()[modal], isOpen: false },
  });
});

it("counts every dialog flag of the UI store as a dialog", () => {
  expect(isAnyModalOpen(useUIStore.getState())).toBe(false);
  useUIStore.setState({ isRenameFolderModalOpen: true });
  expect(isAnyModalOpen(useUIStore.getState())).toBe(true);
  useUIStore.setState({ isRenameFolderModalOpen: false });
});

const pressUndo = () =>
  window.dispatchEvent(
    new KeyboardEvent("keydown", {
      code: "KeyZ",
      key: "z",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    }),
  );

it("undoes the last file operation with Cmd+Z in the Library", () => {
  pressUndo();
  expect(props.handleUndoFileOperation).toHaveBeenCalledOnce();
});

it("redoes the file operation undone last with Shift+Cmd+Z in the Library", () => {
  window.dispatchEvent(
    new KeyboardEvent("keydown", {
      code: "KeyZ",
      key: "z",
      metaKey: true,
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    }),
  );
  expect(props.handleRedoFileOperation).toHaveBeenCalledOnce();
  expect(props.handleUndoFileOperation).not.toHaveBeenCalled();
});

it("leaves Shift+Cmd+Z in Develop to the edit history", () => {
  useUIStore.setState({ activeView: "editor" });
  window.dispatchEvent(
    new KeyboardEvent("keydown", {
      code: "KeyZ",
      key: "z",
      metaKey: true,
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    }),
  );
  expect(props.handleRedoFileOperation).not.toHaveBeenCalled();
});

it("leaves Cmd+Z in Develop to the edit history", () => {
  useUIStore.setState({ activeView: "editor" });
  pressUndo();
  expect(props.handleUndoFileOperation).not.toHaveBeenCalled();
});

it.each([false, true])("Cmd+Z catches the first pending edit (slider focused: %s)", (sliderFocused) => {
  if (sliderFocused) {
    document.body.innerHTML = '<input type="range" aria-label="Face reflections" />';
    document.querySelector('input')!.focus();
  }
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({ selectedImage: {path: "/pending.jpg", isReady: true} as never });
  const initial = {...useEditorStore.getState().adjustments, relightSkinReflections: 0};
  useEditorStore.getState().resetHistory(initial);
  const changed = {...initial, relightSkinReflections: 65};
  useEditorStore.setState({adjustments: changed});
  debouncedSetHistory(changed);
  pressUndo();
  expect(useEditorStore.getState().adjustments).toEqual(initial);
  expect(useEditorStore.getState().historyPending).toBe(false);
  expect(useEditorStore.getState().history).toHaveLength(2);
});

it("Escape in the crop tool restores the crop it opened with, Enter keeps the change", () => {
  const crop = { x: 0, y: 0, width: 100, height: 80, unit: "px" as const };
  useEditorStore.setState({
    selectedImage: { path: "/a.raf", isReady: true } as never,
    adjustments: {
      ...useEditorStore.getState().adjustments,
      crop,
      rotation: 0,
      aspectRatio: null,
    },
  });
  useUIStore.setState({ activeView: "editor", activePanel: Panel.Crop });
  // The photographer crops and turns the frame…
  useEditorStore.setState({
    adjustments: {
      ...useEditorStore.getState().adjustments,
      crop: { ...crop, width: 50 },
      rotation: 3,
    },
  });
  press("Escape");
  expect(useEditorStore.getState().adjustments.crop).toEqual(crop);
  expect(useEditorStore.getState().adjustments.rotation).toBe(0);
  expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
  // Enter keeps the new crop.
  useUIStore.setState({ activePanel: Panel.Crop });
  useEditorStore.setState({
    adjustments: {
      ...useEditorStore.getState().adjustments,
      crop: { ...crop, width: 60 },
    },
  });
  press("Enter");
  expect(useEditorStore.getState().adjustments.crop).toEqual({
    ...crop,
    width: 60,
  });
  expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
});

it("opens the library Metadata tool with I where the library tools are shown", () => {
  const before = useUIStore.getState().activePanel;
  press("KeyI", "i");
  expect(useUIStore.getState().activePanel).toBe(before);
  // The library column as StudioNavigation renders it, the tool closed.
  document.body.innerHTML =
    '<div class="studio-navigation"><details data-tool-title="Metadata"><summary>Metadata</summary></details></div>';
  const tool = document.querySelector("details")!;
  press("KeyI", "i");
  expect(tool.open).toBe(true);
  expect(document.activeElement).toBe(tool.querySelector("summary"));
});

it("toggles Auto Mask with A while a brush component is active (Lightroom)", () => {
  const brush = {
    id: "b",
    type: "brush",
    visible: true,
    mode: "additive",
    parameters: { lines: [] },
  };
  useUIStore.setState({ activeView: "editor", activePanel: Panel.Masks });
  useEditorStore.setState((s) => ({
    selectedImage: {
      path: "/a.raf",
      isReady: true,
      width: 100,
      height: 100,
    } as never,
    adjustments: {
      ...s.adjustments,
      masks: [
        {
          id: "l",
          name: "L",
          visible: true,
          invert: false,
          opacity: 100,
          adjustments: {},
          subMasks: [brush],
        },
      ],
    } as never,
    activeMaskContainerId: "l",
    activeMaskId: "b",
    isWaveformVisible: false,
  }));
  press("KeyA", "a");
  expect(useEditorStore.getState().brushSettings?.autoMask).toBe(true);
  expect(useEditorStore.getState().isWaveformVisible).toBe(false);
  press("KeyA", "a");
  expect(useEditorStore.getState().brushSettings?.autoMask).toBe(false);
});

describe("Delete in Develop", () => {
  const sub = (id: string, type: string) => ({ id, type, visible: true, mode: "additive", parameters: {} });
  const withLayers = (focused: string | null) => {
    useUIStore.setState({ activeView: "editor", activePanel: Panel.Masks });
    useEditorStore.setState((s) => ({
      selectedImage: { path: "/a.raf", isReady: true, width: 100, height: 100 } as never,
      adjustments: {
        ...s.adjustments,
        masks: [
          { id: "l", name: "L", visible: true, invert: false, opacity: 100, adjustments: {}, subMasks: [sub("s", "ai-foreground"), sub("k", "ai-sky")] },
          { id: "m", name: "M", visible: true, invert: false, opacity: 100, adjustments: {}, subMasks: [sub("r", "radial")] },
        ],
      } as never,
      activeMaskContainerId: "l",
      activeMaskId: focused ?? "s",
      focusedMaskComponentId: focused,
    }));
  };
  const masks = () =>
    useEditorStore.getState().adjustments.masks.map((m) => `${m.id}:${m.subMasks.map((x) => x.id).join("")}`);

  it("removes the component picked in the list, and keeps its layer selected", () => {
    withLayers("k");
    press("Backspace");
    expect(masks()).toEqual(["l:s", "m:r"]);
    expect(useEditorStore.getState().activeMaskContainerId).toBe("l");
    expect(props.handleDeleteSelected).not.toHaveBeenCalled();
  });

  it("removes the whole layer when the layer itself is selected", () => {
    withLayers(null);
    press("Backspace");
    expect(masks()).toEqual(["m:r"]);
    expect(useEditorStore.getState().activeMaskContainerId).toBeNull();
  });
});

describe("M after a layer's first adjustment (Lightroom's automatic overlay)", () => {
  const display = () => useSettingsStore.getState().appSettings?.maskDisplay;
  beforeEach(() => {
    useUIStore.setState({ activeView: "editor", activePanel: Panel.Masks });
    useEditorStore.setState({
      selectedImage: { path: "/a.raf", isReady: true, width: 100, height: 100 } as never,
      activeMaskContainerId: "l",
      activeMaskId: "s",
      showMaskWithSelectTool: false,
      maskOverlayAuto: "hidden",
    });
  });

  it("shows the switched-off overlay again and keeps it on, then hides it", () => {
    press("KeyM", "m");
    expect(useEditorStore.getState().maskOverlayAuto).toBe("pinned");
    // The display setting is untouched: the overlay was only switched off.
    expect(display()).toBeUndefined();
    press("KeyM", "m");
    expect(display()).toBe("never");
    press("KeyM", "m");
    expect(display()).toBe("always");
    expect(useEditorStore.getState().maskOverlayAuto).toBe("pinned");
  });

  it("shows it with the select tool too", () => {
    useEditorStore.setState({ activeMaskId: null, showMaskWithSelectTool: true });
    press("KeyM", "m");
    expect(useEditorStore.getState()).toMatchObject({ showMaskWithSelectTool: true, maskOverlayAuto: "pinned" });
    press("KeyM", "m");
    expect(useEditorStore.getState().showMaskWithSelectTool).toBe(false);
  });

  it("ends the luma range eyedropper with Escape", () => {
    useEditorStore.setState({ lumaPickerLayer: "l" });
    press("Escape");
    expect(useEditorStore.getState()).toMatchObject({ lumaPickerLayer: null, activeMaskId: "s" });
  });
});

describe("panel keys (Lightroom's Tab, Shift+Tab, F6–F8)", () => {
  const layout = (module: "library" | "develop") =>
    readModuleLayout(useUIStore.getState().uiVisibility.modules, module);
  const tab = (shiftKey = false) => {
    const event = new KeyboardEvent("keydown", {
      code: "Tab",
      key: "Tab",
      shiftKey,
      bubbles: true,
      cancelable: true,
    });
    (document.activeElement ?? document.body).dispatchEvent(event);
    return event;
  };
  beforeEach(() => {
    useUIStore.setState((s) => ({
      studioToolLayout: "tools-left",
      uiVisibility: { ...s.uiVisibility, modules: undefined },
    }));
  });

  it("hides and shows the side panels with Tab while nothing is focused", () => {
    const event = tab();
    expect(event.defaultPrevented).toBe(true);
    expect(layout("library").left).toBe(false);
    tab();
    expect(layout("library").left).toBe(true);
  });

  it("leaves Tab to focus traversal on fields and controls", () => {
    for (const tag of ["input", "button", "textarea"]) {
      const control = document.createElement(tag);
      document.body.append(control);
      control.focus();
      const event = tab();
      expect(event.defaultPrevented).toBe(false);
      expect(layout("library").left).toBe(true);
      control.remove();
    }
  });

  it("takes Tab on the photograph viewer", () => {
    useUIStore.setState({ activeView: "editor" });
    const viewer = document.createElement("div");
    viewer.tabIndex = 0;
    viewer.dataset.panelShortcut = "true";
    document.body.append(viewer);
    viewer.focus();
    tab();
    expect(layout("develop")).toMatchObject({ left: false, filmstrip: true, bars: true });
  });

  it("hides the filmstrip and the bars too with Shift+Tab, and brings all back", () => {
    useUIStore.setState({ activeView: "editor" });
    tab(true);
    expect(layout("develop")).toEqual({ left: false, right: true, filmstrip: false, bars: false });
    tab(true);
    expect(layout("develop")).toMatchObject({ left: true, filmstrip: true, bars: true });
  });

  it("keeps a layout per module", () => {
    useUIStore.setState({ activeView: "editor" });
    tab(true);
    useUIStore.setState({ activeView: "library" });
    expect(layout("library")).toMatchObject({ left: true, bars: true });
    tab();
    expect(layout("library").left).toBe(false);
    expect(layout("develop").bars).toBe(false);
  });

  it("toggles the left and right panel with F7 and F8 and the filmstrip with F6", () => {
    useUIStore.setState({ activeView: "editor", studioToolLayout: "split-tools" });
    press("F7");
    expect(layout("develop")).toMatchObject({ left: false, right: true });
    press("F8");
    expect(layout("develop")).toMatchObject({ left: false, right: false });
    press("F6");
    expect(layout("develop").filmstrip).toBe(false);
    // Tab with both sides hidden brings both back.
    tab();
    expect(layout("develop")).toMatchObject({ left: true, right: true, filmstrip: false });
  });

  it("follows rebound keys", () => {
    useSettingsStore.setState({
      appSettings: { keybinds: { toggle_side_panels: ["Backslash"] } } as unknown as AppSettings,
    });
    tab();
    expect(layout("library").left).toBe(true);
    press("Backslash", "\\");
    expect(layout("library").left).toBe(false);
  });
});

it("toggles the proof margin with X in Develop and saves it", () => {
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({ selectedImage: { path: "/a.dng" } as never });
  press("KeyX", "x");
  expect(useSettingsStore.getState().appSettings?.viewerProofMargin).toBe(true);
  press("KeyX", "x");
  expect(useSettingsStore.getState().appSettings?.viewerProofMargin).toBe(false);
});

describe("Before/After keys", () => {
  const key = (type: "keydown" | "keyup", code: string, init: KeyboardEventInit = {}) =>
    window.dispatchEvent(new KeyboardEvent(type, { code, bubbles: true, cancelable: true, ...init }));
  beforeEach(() => {
    useUIStore.setState({ activeView: "editor" });
    useEditorStore.setState({
      selectedImage: { path: "/a.raf", isReady: true, width: 100, height: 100 } as never,
      splitView: false,
      showOriginal: false,
      previewOverride: null,
    });
  });

  it("toggles the split with Y, and Shift+Y does nothing", () => {
    key("keydown", "KeyY", { key: "y" });
    expect(useEditorStore.getState().splitView).toBe(true);
    key("keydown", "KeyY", { key: "Y", shiftKey: true });
    expect(useEditorStore.getState().splitView).toBe(true);
    key("keydown", "KeyY", { key: "y" });
    expect(useEditorStore.getState().splitView).toBe(false);
  });

  it("shows the whole photograph before while \\ is held, through auto-repeat", () => {
    key("keydown", "Backslash", { key: "\\" });
    expect(useEditorStore.getState().showOriginal).toBe(true);
    expect(useEditorStore.getState().previewOverride).not.toBeNull();
    key("keydown", "Backslash", { key: "\\", repeat: true });
    key("keydown", "Backslash", { key: "\\", repeat: true });
    expect(useEditorStore.getState().showOriginal).toBe(true);
    // Another key's release leaves it.
    key("keyup", "KeyY", { key: "y" });
    expect(useEditorStore.getState().showOriginal).toBe(true);
    key("keyup", "Backslash", { key: "\\" });
    expect(useEditorStore.getState()).toMatchObject({ showOriginal: false, previewOverride: null });
    // No latched state: a second press is again a held peek.
    key("keydown", "Backslash", { key: "\\" });
    expect(useEditorStore.getState().showOriginal).toBe(true);
    key("keyup", "Backslash", { key: "\\" });
    expect(useEditorStore.getState().showOriginal).toBe(false);
  });

  it("ignores a lone auto-repeat without a first press", () => {
    key("keydown", "Backslash", { key: "\\", repeat: true });
    expect(useEditorStore.getState().showOriginal).toBe(false);
  });

  it("ends the peek when the window loses the focus", () => {
    key("keydown", "Backslash", { key: "\\" });
    expect(useEditorStore.getState().showOriginal).toBe(true);
    window.dispatchEvent(new Event("blur"));
    expect(useEditorStore.getState().showOriginal).toBe(false);
    // The key's later release changes nothing, the next press peeks again.
    key("keyup", "Backslash", { key: "\\" });
    key("keydown", "Backslash", { key: "\\" });
    expect(useEditorStore.getState().showOriginal).toBe(true);
    key("keyup", "Backslash", { key: "\\" });
  });

  it("peeks over the split and returns to it on release", () => {
    useSettingsStore.setState({ osPlatform: "macos" });
    key("keydown", "KeyY", { key: "y" });
    key("keydown", "Backslash", { key: "\\" });
    expect(useEditorStore.getState()).toMatchObject({ splitView: true, showOriginal: true });
    key("keyup", "Backslash", { key: "\\" });
    expect(useEditorStore.getState()).toMatchObject({ splitView: true, showOriginal: false });
  });
});

it("H shows only the selected layer, or the picked component, and H again shows all", () => {
  useUIStore.setState({ activeView: "editor", activePanel: Panel.Masks });
  useEditorStore.setState({
    selectedImage: { path: "/a.dng", isReady: true } as never,
    activeMaskContainerId: "sky",
    focusedMaskComponentId: null,
    layerSolo: null,
  });
  press("KeyH", "h");
  expect(useEditorStore.getState().layerSolo).toEqual({ layerId: "sky", componentId: null });
  press("KeyH", "h");
  expect(useEditorStore.getState().layerSolo).toBeNull();
  useEditorStore.setState({ focusedMaskComponentId: "brush" });
  press("KeyH", "h");
  expect(useEditorStore.getState().layerSolo).toEqual({ layerId: "sky", componentId: "brush" });
  // Without a selected layer, H does nothing.
  useEditorStore.setState({ activeMaskContainerId: null, layerSolo: null });
  press("KeyH", "h");
  expect(useEditorStore.getState().layerSolo).toBeNull();
});
