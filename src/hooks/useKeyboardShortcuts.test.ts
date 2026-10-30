import "../i18n";
import { toast } from "react-toastify";
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useKeyboardShortcuts } from "./useKeyboardShortcuts";
import { useLibraryStore } from "../store/useLibraryStore";
import { useSettingsStore } from "../store/useSettingsStore";
import { isAnyModalOpen, useUIStore } from "../store/useUIStore";
import { AppSettings, Panel } from "../components/ui/AppProperties";
import { useEditorStore, debouncedSetHistory } from "../store/useEditorStore";
import { readModuleLayout } from "../utils/panelLayout";
import { cropGestures } from "../utils/cropGesture";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve()),
}));
vi.mock("react-toastify", () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
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
const press = (code: string, key = code, init: KeyboardEventInit = {}) =>
  window.dispatchEvent(
    new KeyboardEvent("keydown", {
      code,
      key,
      bubbles: true,
      cancelable: true,
      ...init,
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

it("ends the spot tool on Escape in Develop before going back to the Library", () => {
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({ isSpotToolActive: true, activeSpotId: null });
  press("Escape");
  expect(useEditorStore.getState().isSpotToolActive).toBe(false);
  expect(props.handleBackToLibrary).not.toHaveBeenCalled();
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
  const gesturesBefore = cropGestures().started;
  press("Escape");
  expect(useEditorStore.getState().adjustments.crop).toEqual(crop);
  expect(useEditorStore.getState().adjustments.rotation).toBe(0);
  expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
  // A pending Auto straighten answer is for the cancelled edit.
  expect(cropGestures().started).toBe(gesturesBefore + 1);
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
    expect(toast.info).toHaveBeenLastCalledWith("Mask display: Never", expect.objectContaining({ autoClose: 1800 }));
    press("KeyM", "m");
    expect(display()).toBe("always");
    expect(toast.info).toHaveBeenLastCalledWith("Mask display: Always", expect.any(Object));
    expect(useEditorStore.getState().maskOverlayAuto).toBe("pinned");
  });

  it("shows it while the slider just dragged keeps the focus; the slider keeps its arrow keys", () => {
    document.body.innerHTML = '<input type="range" aria-label="Exposure" />';
    document.querySelector("input")!.focus();
    press("ArrowRight");
    expect(useEditorStore.getState().maskOverlayAuto).toBe("hidden");
    press("KeyM", "m");
    expect(useEditorStore.getState().maskOverlayAuto).toBe("pinned");
    document.body.innerHTML = "";
  });

  it("shows it with the select tool too", () => {
    useEditorStore.setState({ activeMaskId: null, showMaskWithSelectTool: true });
    press("KeyM", "m");
    expect(useEditorStore.getState()).toMatchObject({ showMaskWithSelectTool: true, maskOverlayAuto: "pinned" });
    press("KeyM", "m");
    expect(useEditorStore.getState().showMaskWithSelectTool).toBe(false);
  });

  it("announces grayscale and the restored drawing mode with Option+M", () => {
    useSettingsStore.setState((s) => ({
      appSettings: { ...s.appSettings!, maskDisplay: "drawing" },
    }));
    const toggle = () => window.dispatchEvent(new KeyboardEvent("keydown", {
      code: "KeyM", key: "µ", altKey: true, bubbles: true, cancelable: true,
    }));
    toggle();
    expect(display()).toBe("grayscale");
    expect(toast.info).toHaveBeenLastCalledWith("Mask display: Grayscale", expect.any(Object));
    toggle();
    expect(display()).toBe("drawing");
    expect(toast.info).toHaveBeenLastCalledWith("Mask display: While drawing", expect.any(Object));
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

it("toggles the proof margin with Alt+X in Develop and saves it", () => {
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({ selectedImage: { path: "/a.dng" } as never });
  press("KeyX", "x", { altKey: true });
  expect(useSettingsStore.getState().appSettings?.viewerProofMargin).toBe(true);
  press("KeyX", "x", { altKey: true });
  expect(useSettingsStore.getState().appSettings?.viewerProofMargin).toBe(false);
});

it("follows both Caps Lock transitions on macOS", () => {
  const transition = (type: "keydown" | "keyup", locked: boolean) => {
    const event = new KeyboardEvent(type, { code: "CapsLock", key: "CapsLock", bubbles: true, cancelable: true });
    Object.defineProperty(event, "getModifierState", { value: (key: string) => key === "CapsLock" && locked });
    window.dispatchEvent(event);
  };
  transition("keydown", true);
  expect(useSettingsStore.getState().appSettings?.autoAdvance).toBe(true);
  transition("keyup", false);
  expect(useSettingsStore.getState().appSettings?.autoAdvance).toBe(false);
});

it("respects a disabled Caps Lock binding and text fields on macOS", () => {
  useSettingsStore.setState({ appSettings: { keybinds: { toggle_auto_advance: [] }, autoAdvance: false } as unknown as AppSettings });
  const event = new KeyboardEvent("keydown", { code: "CapsLock", bubbles: true });
  Object.defineProperty(event, "getModifierState", { value: () => true });
  window.dispatchEvent(event);
  expect(useSettingsStore.getState().appSettings?.autoAdvance).toBe(false);
  useSettingsStore.setState({ appSettings: { keybinds: {}, autoAdvance: false } as unknown as AppSettings });
  document.body.innerHTML = '<input />';
  document.querySelector("input")!.focus();
  window.dispatchEvent(event);
  expect(useSettingsStore.getState().appSettings?.autoAdvance).toBe(false);
});

it("toggles auto-advance on Caps Lock keydown outside macOS", () => {
  useSettingsStore.setState({ osPlatform: "linux" });
  press("CapsLock");
  expect(useSettingsStore.getState().appSettings?.autoAdvance).toBe(true);
  press("CapsLock");
  expect(useSettingsStore.getState().appSettings?.autoAdvance).toBe(false);
});

describe("Before/After keys", () => {
  const key = (type: "keydown" | "keyup", code: string, init: KeyboardEventInit = {}) =>
    window.dispatchEvent(new KeyboardEvent(type, { code, bubbles: true, cancelable: true, ...init }));
  beforeEach(() => {
    useUIStore.setState({ activeView: "editor" });
    useEditorStore.setState({
      selectedImage: { path: "/a.raf", isReady: true, width: 100, height: 100 } as never,
      splitView: false,
      splitOrientation: 'left-right',
      showOriginal: false,
      previewOverride: null,
    });
  });

  it("toggles with Y and cycles orientations with Shift+Y without resetting zoom or divider", () => {
    useEditorStore.setState({ zoom: 3, splitPosition: 0.3 });
    key("keydown", "KeyY", { key: "y" });
    expect(useEditorStore.getState()).toMatchObject({ splitView: true, splitOrientation: 'left-right' });
    key("keydown", "KeyY", { key: "Y", shiftKey: true });
    expect(useEditorStore.getState()).toMatchObject({ splitView: true, splitOrientation: 'top-bottom' });
    key("keydown", "KeyY", { key: "Y", shiftKey: true, repeat: true });
    expect(useEditorStore.getState().splitView).toBe(true);
    key("keydown", "KeyY", { key: "Y", shiftKey: true });
    expect(useEditorStore.getState().splitView).toBe(false);
    key("keydown", "KeyY", { key: "y" });
    expect(useEditorStore.getState()).toMatchObject({ splitView: true, splitOrientation: 'top-bottom', zoom: 3, splitPosition: 0.3 });
    key("keydown", "KeyY", { key: "Y", shiftKey: true });
    key("keydown", "KeyY", { key: "Y", shiftKey: true });
    expect(useEditorStore.getState()).toMatchObject({ splitView: true, splitOrientation: 'left-right' });
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

it("leaves keys to a focused select or editable text, like to a text field", () => {
  useLibraryStore.setState({ libraryActivePath: "/session/Capture/A.RAF" });
  const select = document.createElement("select");
  select.append(new Option("JPEG"), new Option("TIFF"));
  const editable = document.createElement("div");
  editable.setAttribute("contenteditable", "true");
  editable.tabIndex = 0;
  document.body.append(select, editable);
  for (const control of [select, editable]) {
    control.focus();
    press("Enter");
    expect(props.handleImageSelect).not.toHaveBeenCalled();
  }
  select.blur();
  editable.blur();
  press("Enter");
  expect(props.handleImageSelect).toHaveBeenCalledWith("/session/Capture/A.RAF", true);
});

describe("arrow keys in the Library grid", () => {
  it("move a row with Up/Down, one with Left/Right", async () => {
    const { gridStep } = await import("./useKeyboardShortcuts");
    // 10 photos, 4 per row: rows 0-3, 4-7, 8-9.
    expect(gridStep(1, 10, "ArrowDown", 4)).toBe(5);
    expect(gridStep(5, 10, "ArrowDown", 4)).toBe(9);
    expect(gridStep(7, 10, "ArrowDown", 4)).toBe(9);
    expect(gridStep(9, 10, "ArrowDown", 4)).toBe(9);
    expect(gridStep(5, 10, "ArrowUp", 4)).toBe(1);
    expect(gridStep(2, 10, "ArrowUp", 4)).toBe(2);
    expect(gridStep(3, 10, "ArrowRight", 4)).toBe(4);
    expect(gridStep(9, 10, "ArrowRight", 4)).toBe(0);
    expect(gridStep(0, 10, "ArrowLeft", 4)).toBe(9);
    // No grid shown: Up/Down step one as before.
    expect(gridStep(1, 10, "ArrowDown", 0)).toBe(2);
    expect(gridStep(0, 10, "ArrowUp", 0)).toBe(9);
  });

  it("selects the photo a row below with Down", () => {
    const list = Array.from({ length: 10 }, (_, n) => ({ path: `/p${n}.jpg` })) as never[];
    cleanup();
    renderHook(() => useKeyboardShortcuts({ ...props, sortedImageList: list }));
    useUIStore.setState({ activeView: "library", gridColumns: 4 });
    useLibraryStore.setState({ libraryActivePath: "/p1.jpg", multiSelectedPaths: ["/p1.jpg"] });
    press("ArrowDown");
    expect(useLibraryStore.getState().libraryActivePath).toBe("/p5.jpg");
    expect(props.handleImageSelect).toHaveBeenCalledWith("/p5.jpg", false);
    useUIStore.setState({ gridColumns: 0 });
  });
});

it.each(["Backspace", "Delete"])("opens the Library delete dialog with plain %s on macOS", (code) => {
  useEditorStore.setState({ activeMaskContainerId: null });
  press(code);
  expect(props.handleDeleteSelected).toHaveBeenCalledOnce();
});

it.each(["input", "textarea", "select", "div"])("leaves Delete to a focused %s", (tag) => {
  const field = document.createElement(tag);
  if (tag === "div") { field.contentEditable = "true"; field.setAttribute("contenteditable", "true"); field.tabIndex = 0; }
  document.body.append(field);
  field.focus();
  press("Backspace");
  press("Delete");
  expect(props.handleDeleteSelected).not.toHaveBeenCalled();
});

it("lets a user disable the default Delete action on macOS", () => {
  useSettingsStore.setState({ appSettings: { keybinds: { delete_selected: [] } } as unknown as AppSettings });
  press("Backspace");
  press("Delete");
  expect(props.handleDeleteSelected).not.toHaveBeenCalled();
});

it("does not open photo deletion with plain Backspace in Develop", () => {
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({ activeMaskContainerId: null });
  press("Backspace");
  expect(props.handleDeleteSelected).not.toHaveBeenCalled();
});

it.each(["curve", "regionCurve", "hue", "saturation", "luminance", "grayMixer"] as const)("Escape ends %s targeting while staying in Develop", (tatTarget) => {
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({ tatTarget, isStraightenActive: false, colorPickerTarget: null, isWbPickerActive: false, lumaPickerLayer: null });
  press("Escape");
  expect(useEditorStore.getState().tatTarget).toBeNull();
  expect(props.handleBackToLibrary).not.toHaveBeenCalled();
});

describe("auto-advance after a rating", () => {
  const list = ["/s/a.NEF", "/s/b.NEF", "/s/c.NEF"].map((path) => ({ path }));
  const open = (path: string, autoAdvance?: boolean) => {
    cleanup();
    useSettingsStore.setState({
      appSettings: { keybinds: {}, autoAdvance } as unknown as AppSettings,
      osPlatform: "macos",
    });
    useUIStore.setState({ activeView: "editor" });
    useLibraryStore.setState({ imageList: list as never, multiSelectedPaths: [path], imageRatings: {} });
    useEditorStore.getState().setEditor({ selectedImage: { path, isReady: true } as never });
    renderHook(() => useKeyboardShortcuts({ ...props, sortedImageList: list as never }));
  };

  it("rates the open photograph, then steps to the next one when it is on", () => {
    open("/s/a.NEF", true);
    press("Digit3", "3");
    expect(useLibraryStore.getState().imageRatings["/s/a.NEF"]).toBe(3);
    expect(props.handleImageSelect).toHaveBeenCalledExactlyOnceWith("/s/b.NEF", true);
  });

  it("stays on the photograph when it is off (the default)", () => {
    open("/s/a.NEF");
    press("Digit3", "3");
    expect(useLibraryStore.getState().imageRatings["/s/a.NEF"]).toBe(3);
    expect(props.handleImageSelect).not.toHaveBeenCalled();
  });

  it("does not wrap to the first photograph after the last one", () => {
    open("/s/c.NEF", true);
    press("Digit2", "2");
    expect(useLibraryStore.getState().imageRatings["/s/c.NEF"]).toBe(2);
    expect(props.handleImageSelect).not.toHaveBeenCalled();
  });

  it("steps after a flag key request and a single Library selection", async () => {
    const { requestAutoAdvance } = await import("../utils/photoStepping");
    open("/s/b.NEF", true);
    requestAutoAdvance("/s/b.NEF");
    expect(props.handleImageSelect).toHaveBeenCalledExactlyOnceWith("/s/c.NEF", true);
    vi.clearAllMocks();
    useUIStore.setState({ activeView: "library" });
    useLibraryStore.setState({ libraryActivePath: "/s/b.NEF" });
    requestAutoAdvance("/s/b.NEF");
    expect(props.handleImageSelect).toHaveBeenCalledExactlyOnceWith("/s/c.NEF", false);
  });

  it("rates the selected grid photo and advances to the next one", () => {
    open("/s/a.NEF", true);
    useUIStore.setState({ activeView: "library" });
    useLibraryStore.setState({ libraryActivePath: "/s/a.NEF", multiSelectedPaths: ["/s/a.NEF"] });
    press("Digit3", "3");
    expect(useLibraryStore.getState().imageRatings["/s/a.NEF"]).toBe(3);
    expect(useLibraryStore.getState().multiSelectedPaths).toEqual(["/s/b.NEF"]);
    expect(props.handleImageSelect).toHaveBeenCalledExactlyOnceWith("/s/b.NEF", false);
  });

  it("does not discard a multiple selection when auto-advance is on", async () => {
    const { requestAutoAdvance } = await import("../utils/photoStepping");
    open("/s/a.NEF", true);
    useUIStore.setState({ activeView: "library" });
    useLibraryStore.setState({ libraryActivePath: "/s/a.NEF", multiSelectedPaths: ["/s/a.NEF", "/s/b.NEF"] });
    requestAutoAdvance("/s/a.NEF");
    expect(props.handleImageSelect).not.toHaveBeenCalled();
  });

  it("keeps the Library's loupe and moves its selection on", () => {
    open("/s/a.NEF", true);
    useUIStore.setState({ activeView: "library", libraryLoupe: true });
    press("Digit5", "5");
    expect(useLibraryStore.getState().imageRatings["/s/a.NEF"]).toBe(5);
    expect(props.handleImageSelect).toHaveBeenCalledExactlyOnceWith("/s/b.NEF", false);
    expect(useLibraryStore.getState().multiSelectedPaths).toEqual(["/s/b.NEF"]);
    useUIStore.setState({ libraryLoupe: false });
  });

  const undo = () =>
    window.dispatchEvent(
      new KeyboardEvent("keydown", { code: "KeyZ", key: "z", metaKey: true, bubbles: true, cancelable: true }),
    );

  it("undoes a new edit after auto-advance before the earlier rating", () => {
    open("/s/a.NEF", true);
    useLibraryStore.setState({ imageRatings: { "/s/a.NEF": 2 } });
    press("Digit4", "4");
    expect(props.handleImageSelect).toHaveBeenCalledExactlyOnceWith("/s/b.NEF", true);
    // The step done (what handleImageSelect does to the stores), then Undo.
    useLibraryStore.setState({ multiSelectedPaths: ["/s/b.NEF"] });
    useEditorStore.getState().setEditor({ selectedImage: { path: "/s/b.NEF", isReady: true } as never });
    useEditorStore.setState({ history: [{}] as never, historyIndex: 0, historyPending: false });
    useEditorStore.getState().pushHistory({ exposure: 1 } as never);
    const editUndo = vi.spyOn(useEditorStore.getState(), "undo");
    vi.clearAllMocks();
    undo();
    expect(editUndo).toHaveBeenCalledOnce();
    expect(props.handleImageSelect).not.toHaveBeenCalled();
    expect(useLibraryStore.getState().imageRatings).toMatchObject({ "/s/a.NEF": 4 });
    expect(useLibraryStore.getState().imageRatings["/s/b.NEF"]).toBeUndefined();
  });
});

describe("J switches the clipping warnings", () => {
  const open = () => {
    cleanup();
    useUIStore.setState({ activeView: "editor" });
    useEditorStore.setState({
      selectedImage: { path: "/s/a.NEF", isReady: true } as never,
      clipHighlights: false,
      clipShadows: false,
    });
    renderHook(() => useKeyboardShortcuts(props));
  };

  it("switches both on, and both off again", () => {
    open();
    press("KeyJ", "j");
    expect(useEditorStore.getState().clipHighlights).toBe(true);
    expect(useEditorStore.getState().clipShadows).toBe(true);
    press("KeyJ", "j");
    expect(useEditorStore.getState().clipHighlights).toBe(false);
    expect(useEditorStore.getState().clipShadows).toBe(false);
  });

  it("switches both off after one side was chosen alone", () => {
    open();
    useEditorStore.setState({ clipShadows: true });
    press("KeyJ", "j");
    expect(useEditorStore.getState().clipHighlights).toBe(false);
    expect(useEditorStore.getState().clipShadows).toBe(false);
  });
});
