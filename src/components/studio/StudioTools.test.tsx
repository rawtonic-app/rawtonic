import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  within,
} from "@testing-library/react";
import type { MouseEvent as ReactMouseEvent } from "react";
import StudioWorkspace from "./StudioWorkspace";
import StudioToolBar from "./StudioToolBar";
import { stepBrushSize, useStudioTools } from "./useStudioTools";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { debouncedSetHistory } from "../../hooks/useEditorActions";
import {
  INITIAL_ADJUSTMENTS,
  INITIAL_MASK_CONTAINER,
} from "../../utils/adjustments";
import { createSubMask } from "../../utils/maskUtils";
import { registerSliderDraftRenderer } from "../../utils/sliderDraft";
import { kindOf } from "../../utils/maskKinds";
import { Mask, SubMaskMode, ToolType } from "../panel/right/Masks";
import { Panel, type SelectedImage } from "../ui/AppProperties";

// The viewer stands in for the canvas: a surface that reports right-clicks.
vi.mock("../panel/Editor", () => ({
  default: ({ onContextMenu }: { onContextMenu(e: ReactMouseEvent): void }) => (
    <div data-testid="photo" onContextMenu={onContextMenu} />
  ),
}));
vi.mock("./StudioDevelop", () => ({
  default: () => null,
  ToolGroup: () => null,
}));
vi.mock("./StudioNavigation", () => ({ default: () => null }));

const image = { width: 6000, height: 4000 } as SelectedImage;
beforeEach(() => {
  HTMLDialogElement.prototype.close = vi.fn();
  HTMLDialogElement.prototype.showModal = vi.fn();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  useUIStore.setState({ activeView: "editor", activePanel: Panel.Adjustments });
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    selectedImage: {
      path: "/qa/photo.dng",
      width: 6000,
      height: 4000,
      isRaw: true,
      isReady: true,
    } as SelectedImage,
  });
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
  vi.unstubAllGlobals();
});

const editor = () => useEditorStore.getState();
/** A key press as the browser sends it: down and up on the focused element. */
const press = (key: string, code: string, init: KeyboardEventInit = {}) => {
  fireEvent.keyDown(document.body, { key, code, ...init });
  fireEvent.keyUp(document.body, { key, code, ...init });
};
const pressed = (name: string) =>
  screen.getByRole("button", { name }).getAttribute("aria-pressed");
const withLinearLayer = () => {
  const linear = createSubMask(Mask.Linear, image);
  const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
  adjustments.masks = [
    {
      ...structuredClone(INITIAL_MASK_CONTAINER),
      id: "layer",
      name: "Sky",
      subMasks: [linear],
    },
  ];
  useEditorStore.setState({
    adjustments,
    activeMaskContainerId: "layer",
    activeMaskId: linear.id,
  });
  useUIStore.setState({ activePanel: Panel.Masks });
  return linear;
};
const workspace = (onEditorContextMenu = vi.fn()) => {
  const noop = vi.fn();
  render(
    <div className="studio-app">
      <StudioWorkspace
        files={[]}
        flagCounts={{ all: 0, picked: 0, rejected: 0, unflagged: 0 }}
        transformWrapperRef={{ current: null }}
        onSelect={noop}
        onFilmSelect={noop}
        onOpen={noop}
        onNavigate={noop}
        onBack={noop}
        onRate={noop}
        onContextMenu={noop}
        onEditorContextMenu={onEditorContextMenu}
        onCopy={noop}
        onPaste={noop}
        onReset={noop}
        onZoom={noop}
        requestThumbnails={noop}
        settings={null}
        onSettings={noop}
      />
    </div>,
  );
  return onEditorContextMenu;
};

describe("brush size steps", () => {
  it("change by about 15%, at least one pixel, within 1–6000", () => {
    expect(stepBrushSize(50, 1)).toBe(58);
    expect(stepBrushSize(50, -1)).toBe(42);
    expect(stepBrushSize(1, 1)).toBe(2);
    expect(stepBrushSize(2, -1)).toBe(1);
    expect(stepBrushSize(1, -1)).toBe(1);
    expect(stepBrushSize(5900, 1)).toBe(6000);
  });
});

describe("tool state", () => {
  it("re-renders its users for layer edits, not for the photograph's values", () => {
    withLinearLayer();
    let renders = 0;
    const { result } = renderHook(() => {
      renders++;
      return useStudioTools();
    });
    const before = renders;
    // A slider drag's commits change the photograph's values only.
    act(() =>
      useEditorStore.setState((state) => ({
        adjustments: { ...state.adjustments, exposure: 1 },
      })),
    );
    expect(renders).toBe(before);
    act(() =>
      useEditorStore.setState((state) => ({
        adjustments: {
          ...state.adjustments,
          masks: state.adjustments.masks.map((m) => ({ ...m, invert: true })),
        },
      })),
    );
    expect(renders).toBe(before + 1);
    expect(result.current.layer?.invert).toBe(true);
    expect(result.current.activeTool).toBe("linear");
  });
});

describe("cursor tool bar", () => {
  it("starts a brush layer when no layer is selected and offers erasing only with a layer", () => {
    render(<StudioToolBar />);
    expect(pressed("Select")).toBe("true");
    expect(
      (screen.getByRole("button", { name: "Eraser" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Brush" }));
    const layer = editor().adjustments.masks[0];
    expect(layer.subMasks[0].type).toBe(Mask.Brush);
    expect(editor().activeMaskId).toBe(layer.subMasks[0].id);
    expect(useUIStore.getState().activePanel).toBe(Panel.Masks);
    expect(pressed("Brush")).toBe("true");
  });

  it("draws and erases on the selected layer, keeping its gradient", () => {
    const linear = withLinearLayer();
    render(<StudioToolBar />);
    expect(pressed("Linear gradient")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Eraser" }));
    const layer = editor().adjustments.masks[0];
    expect(layer.subMasks[0]).toEqual(linear);
    expect(
      layer.subMasks.find((m) => m.id === editor().activeMaskId)?.mode,
    ).toBe(SubMaskMode.Subtractive);
    expect(pressed("Eraser")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Select" }));
    expect(editor().activeMaskContainerId).toBe("layer");
    expect(editor().activeMaskId).toBeNull();
    expect(pressed("Select")).toBe("true");
  });

  it("keeps the pen pressure settings when switching to the eraser", () => {
    withLinearLayer();
    useEditorStore.setState({
      brushSettings: { size: 80, feather: 50, tool: ToolType.Brush, pressureSize: false, pressureFlow: true },
    });
    render(<StudioToolBar />);
    fireEvent.click(screen.getByRole("button", { name: "Eraser" }));
    expect(editor().brushSettings).toMatchObject({ pressureSize: false, pressureFlow: true });
  });

  it("switches to crop and back, and the picker works on the photograph", () => {
    withLinearLayer();
    render(<StudioToolBar />);
    fireEvent.click(screen.getByRole("button", { name: "Crop" }));
    expect(useUIStore.getState().activePanel).toBe(Panel.Crop);
    expect(editor().activeMaskContainerId).toBeNull();
    expect(pressed("Crop")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Crop" }));
    expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
    fireEvent.click(
      screen.getByRole("button", { name: "White balance picker" }),
    );
    expect(editor().isWbPickerActive).toBe(true);
    expect(pressed("White balance picker")).toBe("true");
  });

  it("does nothing before the photograph is ready", () => {
    useEditorStore.setState((s) => ({
      selectedImage: { ...s.selectedImage!, isReady: false },
    }));
    render(<StudioToolBar />);
    expect(
      (screen.getByRole("button", { name: "Brush" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });
});

describe("right-click on the photograph", () => {
  it("opens the photograph's menu without a selected layer", () => {
    const menu = workspace();
    fireEvent.contextMenu(screen.getByTestId("photo"), {
      clientX: 400,
      clientY: 300,
    });
    expect(menu).toHaveBeenCalledOnce();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("opens the brush settings at the pointer while painting, and Escape closes only the panel", () => {
    withLinearLayer();
    const menu = workspace();
    fireEvent.click(screen.getByRole("button", { name: "Brush" }));
    fireEvent.contextMenu(screen.getByTestId("photo"), {
      clientX: 400,
      clientY: 300,
    });
    expect(menu).not.toHaveBeenCalled();
    const panel = screen.getByRole("dialog", { name: "Brush settings" });
    expect(panel.textContent).toContain("Sky");
    expect(screen.queryByRole("textbox", { name: "Brush size value (px)" })).toBeNull();
    const size = screen.getByRole("textbox", { name: "Size value (px)" });
    fireEvent.focus(size);
    fireEvent.change(size, { target: { value: "120" } });
    fireEvent.blur(size);
    expect(editor().brushSettings?.size).toBe(120);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(editor().activeMaskContainerId).toBe("layer");
  });

  it("closes on a press outside and when the layer is deselected", () => {
    withLinearLayer();
    workspace();
    fireEvent.contextMenu(screen.getByTestId("photo"), {
      clientX: 10,
      clientY: 10,
    });
    expect(
      screen.getByRole("dialog", { name: "Linear gradient settings" }),
    ).toBeTruthy();
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.contextMenu(screen.getByTestId("photo"), {
      clientX: 10,
      clientY: 10,
    });
    act(() =>
      useEditorStore.setState({
        activeMaskContainerId: null,
        activeMaskId: null,
      }),
    );
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("select tool and closing clicks", () => {
  it("keeps the photograph's menu with the select tool, even with a layer selected", () => {
    withLinearLayer();
    const menu = workspace();
    fireEvent.click(screen.getByRole("button", { name: "Select" }));
    fireEvent.contextMenu(screen.getByTestId("photo"), {
      clientX: 400,
      clientY: 300,
    });
    expect(menu).toHaveBeenCalledOnce();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("lets the press that closes the panel act on the photograph", () => {
    withLinearLayer();
    workspace();
    fireEvent.contextMenu(screen.getByTestId("photo"), { clientX: 10, clientY: 10 });
    const clicked = vi.fn();
    screen.getByTestId("photo").addEventListener("click", clicked);
    fireEvent.pointerDown(screen.getByTestId("photo"));
    fireEvent.click(screen.getByTestId("photo"));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(clicked).toHaveBeenCalledOnce();
  });

  it("closes the photograph's menu with the select tool without the click acting", () => {
    withLinearLayer();
    workspace();
    fireEvent.click(screen.getByRole("button", { name: "Select" }));
    const menu = document.createElement("div");
    menu.setAttribute("role", "menu");
    document.body.append(menu);
    const clicked = vi.fn();
    screen.getByTestId("photo").addEventListener("click", clicked);
    fireEvent.pointerDown(screen.getByTestId("photo"));
    menu.remove();
    fireEvent.mouseDown(screen.getByTestId("photo"));
    fireEvent.click(screen.getByTestId("photo"));
    expect(clicked).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("photo"));
    expect(clicked).toHaveBeenCalledOnce();
  });
});

describe("[ and ]", () => {
  it("resize the active brush and are left to rotation otherwise", () => {
    withLinearLayer();
    workspace();
    const rotate = vi.fn();
    window.addEventListener("keydown", rotate);
    fireEvent.keyDown(document.body, { key: "]" });
    expect(rotate).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Brush" }));
    const before = editor().brushSettings?.size ?? 50;
    fireEvent.keyDown(document.body, { key: "]" });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(before, 1));
    fireEvent.keyDown(document.body, { key: "[" });
    fireEvent.keyDown(document.body, { key: "[" });
    expect(editor().brushSettings?.size).toBeLessThan(before);
    expect(rotate).toHaveBeenCalledOnce();
    window.removeEventListener("keydown", rotate);
  });

  it("resize the brush while the slider just dragged keeps the focus, never rotating", () => {
    withLinearLayer();
    workspace();
    fireEvent.click(screen.getByRole("button", { name: "Brush" }));
    const rotate = vi.fn();
    window.addEventListener("keydown", rotate);
    const slider = document.body.appendChild(document.createElement("input"));
    slider.type = "range";
    slider.focus();
    const before = editor().brushSettings?.size ?? 50;
    fireEvent.keyDown(slider, { key: "]", code: "BracketRight" });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(before, 1));
    expect(rotate).not.toHaveBeenCalled();
    // The slider's own keys stay with it.
    fireEvent.keyDown(slider, { key: "ArrowRight", code: "ArrowRight" });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(before, 1));
    window.removeEventListener("keydown", rotate);
    slider.remove();
  });
});

describe("Capture One tool keys", () => {
  it("pick the cursor tools in Develop", () => {
    workspace();
    press("b", "KeyB");
    expect(pressed("Brush")).toBe("true");
    const layer = editor().activeMaskContainerId;
    expect(layer).toBeTruthy();
    press("e", "KeyE");
    expect(pressed("Eraser")).toBe("true");
    expect(editor().activeMaskContainerId).toBe(layer);
    press("v", "KeyV");
    expect(pressed("Select")).toBe("true");
    press("l", "KeyL");
    expect(pressed("Linear gradient")).toBe("true");
    press("t", "KeyT");
    expect(pressed("Radial gradient")).toBe("true");
    expect(editor().adjustments.masks).toHaveLength(3);
    press("c", "KeyC");
    expect(useUIStore.getState().activePanel).toBe(Panel.Crop);
    // R is Capture One's straighten tool (in the crop view); again, crop.
    press("r", "KeyR");
    expect(pressed("Straighten")).toBe("true");
    expect(editor().isStraightenActive).toBe(true);
    press("r", "KeyR");
    expect(pressed("Crop")).toBe("true");
    press("c", "KeyC");
    expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
    press("w", "KeyW");
    expect(editor().isWbPickerActive).toBe(true);
  });

  it("leave the Library, modified keys and text fields alone", () => {
    workspace();
    press("b", "KeyB", { metaKey: true });
    expect(editor().adjustments.masks).toHaveLength(0);
    const field = document.createElement("input");
    document.body.appendChild(field);
    fireEvent.keyDown(field, { key: "b", code: "KeyB" });
    fireEvent.keyUp(field, { key: "b", code: "KeyB" });
    expect(editor().adjustments.masks).toHaveLength(0);
    field.remove();
    act(() => useUIStore.setState({ activeView: "library" }));
    press("b", "KeyB");
    expect(editor().adjustments.masks).toHaveLength(0);
  });

  it("follow the user's own keybinds", async () => {
    const { useSettingsStore } = await import("../../store/useSettingsStore");
    useSettingsStore.setState({
      appSettings: { keybinds: { tool_brush: ["KeyN"] } } as never,
    });
    try {
      workspace();
      press("b", "KeyB");
      press("k", "KeyK");
      expect(editor().adjustments.masks).toHaveLength(0);
      press("n", "KeyN");
      expect(pressed("Brush")).toBe("true");
    } finally {
      useSettingsStore.setState({ appSettings: null });
    }
  });

  it("start a Subject or Background layer with Shift+, and Shift+.", () => {
    workspace();
    // German Mac layouts type ; and : there: the key's code decides.
    press(";", "Comma", { shiftKey: true });
    press(":", "Period", { shiftKey: true });
    const masks = editor().adjustments.masks;
    expect(masks).toHaveLength(2);
    expect(masks.map((m) => kindOf(m.subMasks[0]))).toEqual(["subject", "background"]);
    expect(editor().activeMaskContainerId).toBe(masks[1].id);
    expect(useUIStore.getState().activePanel).toBe(Panel.Masks);
  });

  it("change brush hardness with Shift and accept Option-typed brackets", () => {
    workspace();
    fireEvent.click(screen.getByRole("button", { name: "Brush" }));
    const { size, hardness = 50 } = editor().brushSettings!;
    fireEvent.keyDown(document.body, { key: "}", shiftKey: true });
    expect(editor().brushSettings?.hardness).toBe(hardness + 10);
    expect(editor().brushSettings?.size).toBe(size);
    // German Mac layouts type ] as Option+6.
    fireEvent.keyDown(document.body, { key: "]", altKey: true });
    expect(editor().brushSettings?.size).toBe(stepBrushSize(size, 1));
  });
});

describe("layer selector in the viewer bar", () => {
  it("picks the layer the tools act on, and Background returns to the photograph", () => {
    withLinearLayer();
    useEditorStore.setState({ activeMaskContainerId: null, activeMaskId: null });
    useUIStore.setState({ activePanel: Panel.Adjustments });
    workspace();
    const select = screen.getByRole("combobox", { name: "Layer" }) as HTMLSelectElement;
    expect(select.value).toBe("");
    fireEvent.change(select, { target: { value: "layer" } });
    expect(editor().activeMaskContainerId).toBe("layer");
    expect(editor().activeMaskId).toBeNull();
    expect(useUIStore.getState().activePanel).toBe(Panel.Masks);
    expect(select.value).toBe("layer");
    fireEvent.change(select, { target: { value: "" } });
    expect(editor().activeMaskContainerId).toBeNull();
    expect(select.value).toBe("");
  });
});

describe("crop tool panel", () => {
  it("opens on right-click with the crop tool, swaps orientation and resets", () => {
    const menu = workspace();
    fireEvent.click(screen.getByRole("button", { name: "Crop" }));
    fireEvent.contextMenu(screen.getByTestId("photo"), { clientX: 400, clientY: 300 });
    expect(menu).not.toHaveBeenCalled();
    const panel = screen.getByRole("dialog", { name: "Crop settings" });
    expect(panel.textContent).toContain("6000 × 4000 px");
    const ratio = screen.getByRole("combobox", { name: "Crop aspect ratio" });
    fireEvent.change(ratio, { target: { value: "1.5" } });
    expect(editor().adjustments.aspectRatio).toBe(1.5);
    fireEvent.click(screen.getByRole("button", { name: "Portrait" }));
    expect(editor().adjustments.aspectRatio).toBeCloseTo(2 / 3);
    // A new ratio keeps the portrait orientation.
    fireEvent.change(ratio, { target: { value: String(16 / 9) } });
    expect(editor().adjustments.aspectRatio).toBeCloseTo(9 / 16);
    fireEvent.click(within(panel).getByRole("button", { name: "Reset crop and straighten" }));
    expect(editor().adjustments.aspectRatio).toBeNull();
    expect(editor().adjustments.crop).toBeNull();
  });

  it("opens with the pointer in its title bar", () => {
    workspace();
    fireEvent.click(screen.getByRole("button", { name: "Crop" }));
    fireEvent.contextMenu(screen.getByTestId("photo"), { clientX: 400, clientY: 300 });
    const panel = screen.getByRole("dialog", { name: "Crop settings" });
    // jsdom has no layout: the title is 0 px high, so the pointer is at its top.
    expect(panel.style.left).toBe("368px");
    expect(panel.style.top).toBe("300px");
  });
});

describe("new gradients", () => {
  it("wait for a drag on the photo to draw them, outside the recipe", () => {
    workspace();
    useUIStore.setState({ activePanel: Panel.Masks });
    press("l", "KeyL");
    const linear = editor().adjustments.masks[0].subMasks[0];
    expect(linear.type).toBe(Mask.Linear);
    // The next drag draws it (notes: reference/wp7-use-test-c1-lr.md finding 2).
    expect(editor().shapeDrawTarget).toBe(linear.id);
    expect(Object.keys(linear.parameters)).not.toContain("isInitialDraw");
    press("t", "KeyT");
    const radial = editor().adjustments.masks[1].subMasks[0];
    expect(editor().shapeDrawTarget).toBe(radial.id);
    // Picking another component ends it; a brush never waits for a drag.
    useEditorStore.setState({ activeMaskId: linear.id });
    expect(editor().shapeDrawTarget).toBeNull();
    press("b", "KeyB");
    expect(editor().shapeDrawTarget).toBeNull();
  });
});

describe("radial panel", () => {
  it("draws the mask inside or outside", () => {
    const radial = createSubMask(Mask.Radial, image);
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.masks = [
      { ...structuredClone(INITIAL_MASK_CONTAINER), id: "r", name: "Spot", subMasks: [radial] },
    ];
    useEditorStore.setState({ adjustments, activeMaskContainerId: "r", activeMaskId: radial.id });
    useUIStore.setState({ activePanel: Panel.Masks });
    workspace();
    fireEvent.contextMenu(screen.getByTestId("photo"), { clientX: 400, clientY: 300 });
    const inside = screen.getByRole("checkbox", { name: "Draw mask inside" }) as HTMLInputElement;
    expect(inside.checked).toBe(!radial.invert);
    fireEvent.click(inside);
    expect(editor().adjustments.masks[0].subMasks[0].invert).toBe(!radial.invert ? true : false);
  });
});

describe("Quick Adjust (Capture One)", () => {
  it("changes a control while its key is held and the wheel turns over the photo", () => {
    workspace();
    const photo = screen.getByTestId("photo");
    fireEvent.keyDown(document.body, { key: "q", code: "KeyQ" });
    fireEvent.wheel(photo, { deltaY: -100, clientX: 300, clientY: 200 });
    fireEvent.wheel(photo, { deltaY: -100, clientX: 300, clientY: 200 });
    expect(editor().adjustments.exposure).toBeCloseTo(0.1);
    expect(screen.getByRole("status").textContent).toBe("Exposure+0.1");
    expect(editor().isSliderDragging).toBe(true);
    fireEvent.keyUp(document.body, { key: "q", code: "KeyQ" });
    expect(screen.queryByRole("status")).toBeNull();
    expect(editor().isSliderDragging).toBe(false);
  });

  it("adds up wheel steps that come faster than the drag's store commits", () => {
    // As in the app: the preview renders the moves between commits as a draft.
    const stop = registerSliderDraftRenderer(() => true);
    try {
      workspace();
      const photo = screen.getByTestId("photo");
      fireEvent.keyDown(document.body, { key: "q", code: "KeyQ" });
      fireEvent.wheel(photo, { deltaY: -100, clientX: 300, clientY: 200 });
      fireEvent.wheel(photo, { deltaY: -100, clientX: 300, clientY: 200 });
      expect(screen.getByRole("status").textContent).toBe("Exposure+0.1");
      fireEvent.keyUp(document.body, { key: "q", code: "KeyQ" });
      expect(editor().adjustments.exposure).toBeCloseTo(0.1);
    } finally {
      stop();
    }
  });

  it("keeps a tapped key's own action and drags change the value", () => {
    workspace();
    const photo = screen.getByTestId("photo");
    // A tap on W still picks the white balance tool.
    press("w", "KeyW");
    expect(editor().isWbPickerActive).toBe(true);
    press("w", "KeyW");
    expect(editor().isWbPickerActive).toBe(false);
    // Held W with a drag changes Contrast instead.
    fireEvent.keyDown(document.body, { key: "w", code: "KeyW" });
    fireEvent.pointerDown(photo, { button: 0, clientX: 100, clientY: 100 });
    fireEvent.pointerMove(photo, { clientX: 140, clientY: 100 });
    fireEvent.pointerUp(photo, { clientX: 140, clientY: 100 });
    fireEvent.keyUp(document.body, { key: "w", code: "KeyW" });
    expect(editor().adjustments.contrast).toBe(10);
    expect(editor().isWbPickerActive).toBe(false);
  });

  it("acts on the selected layer", () => {
    withLinearLayer();
    workspace();
    const photo = screen.getByTestId("photo");
    fireEvent.keyDown(document.body, { key: "r", code: "KeyR" });
    fireEvent.wheel(photo, { deltaY: -100 });
    fireEvent.keyUp(document.body, { key: "r", code: "KeyR" });
    expect(editor().adjustments.masks[0].adjustments.saturation).toBe(1);
    expect(editor().adjustments.saturation).toBe(0);
  });
});

it("puts the cursor into the Library search field on its shortcut", () => {
  useUIStore.setState({ activeView: "library" });
  workspace();
  const search = screen.getByRole("searchbox");
  act(() => useUIStore.getState().requestSearchFocus());
  expect(document.activeElement).toBe(search);
});


describe("workspace keyboard accessibility", () => {
  it("anchors keyboard tool settings at the viewer and returns focus on Escape", () => {
    workspace();
    act(() => useUIStore.setState({ activePanel: Panel.Crop }));
    const photo = screen.getByTestId("photo");
    photo.tabIndex = 0;
    photo.focus();
    vi.spyOn(photo, "getBoundingClientRect").mockReturnValue({ left: 400, bottom: 300 } as DOMRect);
    expect(pressed("Crop")).toBe("true");
    fireEvent.contextMenu(photo, { clientX: 0, clientY: 0 });
    const panel = screen.getByRole("dialog");
    expect(parseFloat(panel.style.left)).toBeGreaterThan(300);
    expect(panel.contains(document.activeElement)).toBe(true);
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(photo);
  });

  it.each(["split-tools", "tools-left"] as const)("moves the separator in the arrow direction in %s", (layout) => {
    useUIStore.getState().setStudioToolLayout(layout);
    workspace();
    const separator = screen.getByRole("separator", { name: "Tool panel width" });
    const before = Number(separator.getAttribute("aria-valuenow"));
    fireEvent.keyDown(separator, { key: "ArrowRight" });
    expect(Number(separator.getAttribute("aria-valuenow"))).toBe(before + (layout === "split-tools" ? -10 : 10));
    fireEvent.keyDown(separator, { key: "ArrowLeft" });
    expect(Number(separator.getAttribute("aria-valuenow"))).toBe(before);
  });

it("opens tool settings without focusing Reset", () => {
  withLinearLayer();
  const brush = createSubMask(Mask.Brush, image);
  useEditorStore.setState((state) => ({
    adjustments: { ...state.adjustments, masks: state.adjustments.masks.map((mask) => ({ ...mask, subMasks: [brush] })) },
    activeMaskId: brush.id,
  }));
  workspace();
  fireEvent.contextMenu(screen.getByTestId("photo"), { clientX: 400, clientY: 300 });
  const panel = screen.getByRole("dialog", { name: "Brush settings" });
  expect(panel.contains(document.activeElement)).toBe(true);
  expect(document.activeElement?.getAttribute("aria-label")).not.toBe("Reset brush");
  expect(document.activeElement?.classList.contains("studio-tool-popover-reset")).toBe(false);
});

it.each(["ContextMenu", "F10"])("opens brush settings from the existing viewer with %s", (key) => {
  withLinearLayer();
  const brush = createSubMask(Mask.Brush, image);
  useEditorStore.setState((state) => ({
    adjustments: { ...state.adjustments, masks: state.adjustments.masks.map((mask) => ({ ...mask, subMasks: [brush] })) },
    activeMaskId: brush.id,
  }));
  workspace();
  const viewer = screen.getByRole("group", { name: "Image viewer" });
  viewer.focus();
  vi.spyOn(viewer, "getBoundingClientRect").mockReturnValue({ left: 400, bottom: 300 } as DOMRect);
  fireEvent.keyDown(viewer, { key, shiftKey: key === "F10" });
  const panel = screen.getByRole("dialog", { name: "Brush settings" });
  expect(parseFloat(panel.style.left)).toBeGreaterThan(300);
  expect(panel.contains(document.activeElement)).toBe(true);
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(document.activeElement).toBe(viewer);
});

});
