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
import StudioDevelop from "./StudioDevelop";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { useProcessStore } from "../../store/useProcessStore";
import { debouncedSetHistory, useEditorActions } from "../../hooks/useEditorActions";
import { COLOR_NOISE_VERSION, INITIAL_ADJUSTMENTS, normalizeLoadedAdjustments } from "../../utils/adjustments";
import { Panel, type AppSettings, type SelectedImage } from "../ui/AppProperties";
import { Mask, SubMaskMode } from "../panel/right/Masks";
import { useSettingsStore } from "../../store/useSettingsStore";
import { ContextMenuProvider } from "../../context/ContextMenuContext";
import { createSubMask } from "../../utils/maskUtils";
import { INITIAL_MASK_CONTAINER } from "../../utils/adjustments";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((name: string) => Promise.resolve(name === "load_metadata" ? {} : undefined)),
}));

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("requestAnimationFrame", (fn: FrameRequestCallback) =>
    setTimeout(() => fn(0), 16),
  );
  vi.stubGlobal("cancelAnimationFrame", clearTimeout);
  vi.stubGlobal("PointerEvent", MouseEvent);
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
  useUIStore.setState({ activePanel: Panel.Adjustments });
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
const mount = () => render(<StudioDevelop />);
/** The Layers tab, where the New layer grid shows while there are none. */
const openLayersTab = () =>
  fireEvent.click(
    within(screen.getByRole("navigation")).getByRole("button", {
      name: "Layers & masks",
    }),
  );
const openGroups = () =>
  document.querySelectorAll("details").forEach((group) => {
    group.open = true;
  });
const edit = (label: string, value: number) => {
  const input = screen.getByRole("textbox", { name: `${label} value` });
  fireEvent.focus(input);
  fireEvent.change(input, { target: { value: String(value) } });
  fireEvent.blur(input);
};

describe("Develop tool contracts", () => {
  it("accepts adjustments only after the selected source and metadata are ready", () => {
    useEditorStore.setState((state) => ({ selectedImage: { ...state.selectedImage!, isReady: false } }));
    const before = useEditorStore.getState().adjustments;
    const { result } = renderHook(() => useEditorActions());
    mount();
    openGroups();
    expect((screen.getByRole("textbox", { name: "Exposure value" }) as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Auto" }) as HTMLButtonElement).disabled).toBe(true);
    act(() => result.current.setAdjustments({ exposure: 3 }));
    act(() => result.current.handleRotate(90));
    expect(useEditorStore.getState().adjustments).toBe(before);
    act(() => useEditorStore.setState((state) => ({ selectedImage: { ...state.selectedImage!, isReady: true } })));
    edit("Exposure", 1.5);
    expect(useEditorStore.getState().adjustments.exposure).toBe(1.5);
  });
  it("exposes inherited section bypasses and allows re-enabling them", () => {
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.sectionVisibility.details = false;
    adjustments.sectionVisibility.basic = false;
    adjustments.exposure = 1;
    useEditorStore.setState({ adjustments });
    mount();
    // Master switches without a tool eye are listed; Exposure shows its eye.
    expect(screen.getByText("Detail bypassed")).toBeTruthy();
    expect(screen.queryByText("Exposure bypassed")).toBeNull();
    expect(screen.getByRole("button", { name: "Exposure on" }).getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(screen.getByRole("button", { name: "Enable Detail" }));
    expect(useEditorStore.getState().adjustments.sectionVisibility.details).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Exposure on" }));
    expect(useEditorStore.getState().adjustments.sectionVisibility.basic).toBe(true);
    expect(useEditorStore.getState().adjustments.exposure).toBe(1);
  });
  it("matches the processing units for every primary image slider", () => {
    mount();
    openGroups();
    const expected: [string, number, number, number, number][] = [
      ["Temperature", -100, 100, 1, 0],
      ["Tint", -100, 100, 1, 0],
      ["Exposure", -5, 5, 0.01, 0],
      ["Brightness", -5, 5, 0.01, 0],
      ...[
        "Contrast",
        "Highlights",
        "Shadows",
        "Whites",
        "Blacks",
        "Clarity",
        "Structure",
        "Dehaze",
        "Vibrance",
        "Saturation",
        "Amount",
        "Vignette",
      ].map((label): [string, number, number, number, number] => [
        label,
        -100,
        100,
        1,
        0,
      ]),
      ["Threshold", 0, 80, 1, 15],
      ["Luminance", 0, 100, 1, 0],
      ["Color", 0, 100, 1, 0],
      ["Midpoint", 0, 100, 1, 50],
      ["Feather", 0, 100, 1, 50],
      ["Grain", 0, 100, 1, 0],
      ["Size", 0, 100, 1, 25],
    ];
    // Each slider lives in one tab; names overlap (noise Luminance/Color,
    // sharpening and moiré Amount), so any matching control counts.
    const found = new Set<string>();
    for (const tabName of ["Adjustments", "Detail"]) {
      fireEvent.click(
        within(screen.getByRole("navigation")).getByRole("button", { name: tabName }),
      );
      openGroups();
      for (const [label, min, max, step, initial] of expected) {
        const controls = screen.queryAllByRole("slider", {
          name: label,
        }) as HTMLInputElement[];
        if (
          controls.some(
            (s) =>
              +s.min === min &&
              +s.max === max &&
              +s.step === step &&
              +s.value === initial,
          )
        )
          found.add(label);
      }
    }
    expect(expected.map(([label]) => label).filter((l) => !found.has(l))).toEqual([]);
    fireEvent.click(screen.getByRole("button", { name: "Crop, geometry & lens" }));
    for (const [label, min, max, step, initial] of [
      ["Straighten", -45, 45, 0.1, 0],
      ["Rotation", -45, 45, 0.1, 0],
      ["Vertical", -100, 100, 1, 0],
      ["Horizontal", -100, 100, 1, 0],
      ["Aspect", -100, 100, 1, 0],
      ["Scale", 50, 150, 1, 100],
    ] as const) {
      const slider = screen.getByRole("slider", {
        name: label,
      }) as HTMLInputElement;
      expect([+slider.min, +slider.max, +slider.step, +slider.value]).toEqual([
        min,
        max,
        step,
        initial,
      ]);
    }
  });

  it("offers sharpening amounts to 150 from response 2 on, layers to 100", () => {
    const sharpening = () => {
      fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Detail" }));
      openGroups();
      return within(screen.getByRole("slider", { name: "Threshold" }).closest("details")!);
    };
    const amountMax = () => +(sharpening().getByRole("slider", { name: "Amount" }) as HTMLInputElement).max;
    useEditorStore.setState({ adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), sharpenVersion: 5 } });
    mount();
    expect(amountMax()).toBe(150);
    const input = sharpening().getByRole("textbox", { name: "Amount value" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "140" } });
    fireEvent.blur(input);
    expect(useEditorStore.getState().adjustments.sharpness).toBe(140);
    cleanup();
    // Response 1 (recipes without the key): its halos grew with the amount.
    useEditorStore.setState({ adjustments: structuredClone(INITIAL_ADJUSTMENTS) });
    mount();
    expect(amountMax()).toBe(100);
    cleanup();
    useEditorStore.setState({ adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), sharpenVersion: 5 } });
    mount();
    fireEvent.click(screen.getByRole("button", { name: "Layers & masks" }));
    fireEvent.click(screen.getByRole("button", { name: "New linear gradient layer" }));
    expect(amountMax()).toBe(100);
  });

  it("makes brightness fine-grained, preserves stored units and isolates local edits", () => {
    useEditorStore.setState({
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), brightness: 0.4 },
    });
    mount();
    const slider = screen.getByRole("slider", { name: "Brightness" });
    expect((slider as HTMLInputElement).value).toBe("0.4");
    fireEvent.keyDown(slider, { key: "ArrowRight" });
    expect(useEditorStore.getState().adjustments.brightness).toBe(0.41);
    edit("Brightness", 80);
    expect(useEditorStore.getState().adjustments.brightness).toBe(5);
    fireEvent.doubleClick(slider);
    expect(useEditorStore.getState().adjustments.brightness).toBe(0);
    fireEvent.click(screen.getByRole("button", { name: "Layers & masks" }));
    fireEvent.click(screen.getByRole("button", { name: "New linear gradient layer" }));
    edit("Brightness", -0.25);
    const { adjustments } = useEditorStore.getState();
    expect(adjustments.brightness).toBe(0);
    expect(adjustments.masks[0].adjustments.brightness).toBe(-0.25);
    expect(screen.queryByRole("slider", { name: "Brush size" })).toBeNull();
    expect(
      (
        screen.getByRole("button", {
          name: "White balance picker",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    // Auto adjusts the photograph, so a layer's Exposure tool has none.
    expect(screen.queryByRole("button", { name: "Auto" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Reset Exposure" }));
    expect(
      useEditorStore.getState().adjustments.masks[0].adjustments.brightness,
    ).toBe(0);
  });

  it("resets all visible effects, including vignette midpoint and feather", () => {
    mount();
    openGroups();
    edit("Midpoint", 20);
    edit("Feather", 90);
    edit("Size", 70);
    edit("Grain", 40);
    edit("Vignette", -25);
    const structure = screen.getByRole("combobox", { name: "Grain structure" }) as HTMLSelectElement;
    // Grain turned on in a new edit starts as Clumped.
    expect(structure.value).toBe("clumped");
    expect(useEditorStore.getState().adjustments.grainStructure).toBe("clumped");
    fireEvent.change(structure, { target: { value: "gritty" } });
    expect(useEditorStore.getState().adjustments.grainStructure).toBe("gritty");
    fireEvent.click(screen.getByRole("button", { name: "Reset Effects" }));
    const a = useEditorStore.getState().adjustments;
    expect([
      a.vignetteAmount,
      a.vignetteMidpoint,
      a.vignetteFeather,
      a.grainAmount,
      a.grainSize,
      a.grainStructure,
    ]).toEqual([0, 50, 50, 0, 25, undefined]);
    // After a reset, grain turned on again starts as a new edit's.
    edit("Grain", 30);
    expect(useEditorStore.getState().adjustments.grainStructure).toBe("clumped");
  });

  it("keeps the Even grain of recipes from before the structure choice", () => {
    useEditorStore.setState({
      adjustments: normalizeLoadedAdjustments(
        { ...structuredClone(INITIAL_ADJUSTMENTS), grainAmount: 40, grainStructure: undefined },
        true,
      ),
    });
    mount();
    openGroups();
    const structure = screen.getByRole("combobox", { name: "Grain structure" }) as HTMLSelectElement;
    expect(structure.value).toBe("even");
    edit("Grain", 0);
    edit("Grain", 25);
    expect(useEditorStore.getState().adjustments.grainStructure).toBe("even");
  });

  it("marks mixer and opacity drags interactive and flushes release", () => {
    mount();
    fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Color" }));
    openGroups();
    const hue = screen.getByRole("slider", { name: "Hue" });
    fireEvent.pointerDown(hue);
    expect(useEditorStore.getState().isSliderDragging).toBe(true);
    fireEvent.change(hue, { target: { value: 25 } });
    fireEvent.pointerUp(hue);
    expect(useEditorStore.getState().isSliderDragging).toBe(false);
    expect(useEditorStore.getState().adjustments.hsl.reds.hue).toBe(25);
    fireEvent.click(screen.getByRole("button", { name: "Layers & masks" }));
    fireEvent.click(screen.getByRole("button", { name: "New radial gradient layer" }));
    const opacity = screen.getByRole("slider", { name: "Opacity" });
    fireEvent.pointerDown(opacity);
    expect(useEditorStore.getState().isSliderDragging).toBe(true);
    fireEvent.change(opacity, { target: { value: 35 } });
    fireEvent.pointerCancel(opacity);
    expect(useEditorStore.getState().adjustments.masks[0].opacity).toBe(35);
    expect(useEditorStore.getState().isSliderDragging).toBe(false);
  });

  it("applies a real crop immediately and preserves it when unlocking its ratio", () => {
    mount();
    fireEvent.click(screen.getByRole("button", { name: "Crop, geometry & lens" }));
    const ratio = screen.getByRole("combobox", { name: "Crop aspect ratio" });
    fireEvent.change(ratio, { target: { value: "1" } });
    expect(useEditorStore.getState().adjustments.crop).toEqual({
      unit: "px",
      x: 1000,
      y: 0,
      width: 4000,
      height: 4000,
    });
    fireEvent.change(ratio, { target: { value: "0" } });
    expect(useEditorStore.getState().adjustments.crop?.width).toBe(4000);
    fireEvent.click(screen.getByRole("button", { name: "Reset Crop" }));
    expect(useEditorStore.getState().adjustments.crop).toBeNull();
  });

  it("does not permit a seventeenth GPU curve point", () => {
    const curves = structuredClone(INITIAL_ADJUSTMENTS.curves);
    curves.luma = [
      ...Array.from({ length: 15 }, (_, x) => ({ x, y: x })),
      { x: 255, y: 255 },
    ];
    useEditorStore.setState({
      adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), curves },
    });
    mount();
    openGroups();
    const curve = screen.getByRole("img", { name: /^RGB point curve/ });
    vi.spyOn(curve, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      width: 255,
      height: 255,
    } as DOMRect);
    fireEvent.pointerDown(curve, { button: 0, clientX: 100, clientY: 200 });
    expect(useEditorStore.getState().adjustments.curves.luma).toHaveLength(16);
    expect(useEditorStore.getState().isSliderDragging).toBe(false);
  });

  it("edits the Luminance curve in its own channel", () => {
    mount();
    openGroups();
    fireEvent.click(screen.getByRole("button", { name: "Luminance" }));
    const curve = screen.getByRole("img", { name: /^luminance point curve/ });
    vi.spyOn(curve, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      width: 255,
      height: 255,
    } as DOMRect);
    curve.setPointerCapture = vi.fn();
    fireEvent.pointerDown(curve, { button: 0, clientX: 100, clientY: 135 });
    const { curves } = useEditorStore.getState().adjustments;
    // A new point on a neutral Luminance curve; the RGB curve stays.
    expect(curves.luminance).toEqual([
      { x: 0, y: 0 },
      { x: 100, y: 120 },
      { x: 255, y: 255 },
    ]);
    expect(curves.luma).toEqual(INITIAL_ADJUSTMENTS.curves.luma);
    fireEvent.pointerCancel(curve);
  });

  it("draws a long Luminance curve as it renders, cut after 16 points", () => {
    // Twenty points (an import); the renderer takes the first 16, ending
    // at (150, 150), so the drawn curve holds 150 from there.
    const luminance = [
      ...Array.from({ length: 16 }, (_, i) => ({ x: i * 10, y: i * 10 })),
      { x: 200, y: 240 },
      { x: 220, y: 250 },
      { x: 240, y: 252 },
      { x: 255, y: 255 },
    ];
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        curves: { ...INITIAL_ADJUSTMENTS.curves, luminance, version: 3 },
      },
    });
    mount();
    openGroups();
    fireEvent.click(screen.getByRole("button", { name: "Luminance" }));
    const curve = screen.getByRole("img", { name: /^luminance point curve/ });
    const line = curve.querySelector("polyline.curve-line")!;
    const drawn = line
      .getAttribute("points")!
      .split(" ")
      .map((p) => p.split(",").map(Number));
    const [, y] = drawn[drawn.length - 1];
    expect(255 - y).toBeCloseTo(150, 5);
    cleanup();
    // A long curve that changes nothing renders as none: drawn neutral,
    // not cut after its 16th point.
    const identity = Array.from({ length: 20 }, (_, i) => ({ x: (i * 255) / 19, y: (i * 255) / 19 }));
    useEditorStore.setState({
      adjustments: {
        ...structuredClone(INITIAL_ADJUSTMENTS),
        curves: { ...INITIAL_ADJUSTMENTS.curves, luminance: identity, version: 3 },
      },
    });
    mount();
    openGroups();
    fireEvent.click(screen.getByRole("button", { name: "Luminance" }));
    const neutral = screen
      .getByRole("img", { name: /^luminance point curve/ })
      .querySelector("polyline.curve-line")!
      .getAttribute("points")!
      .split(" ")
      .map((p) => p.split(",").map(Number));
    for (const [x, py] of neutral) expect(255 - py).toBeCloseTo(x, 5);
  });

  it("ends a curve drag on cancellation and unmount", () => {
    const view = mount();
    openGroups();
    const curve = screen.getByRole("img", { name: /^RGB point curve/ });
    vi.spyOn(curve, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      width: 255,
      height: 255,
    } as DOMRect);
    curve.setPointerCapture = vi.fn();
    fireEvent.pointerDown(curve, { button: 0, clientX: 120, clientY: 120 });
    expect(useEditorStore.getState().isSliderDragging).toBe(true);
    fireEvent.pointerCancel(curve);
    expect(useEditorStore.getState().isSliderDragging).toBe(false);
    fireEvent.pointerDown(curve, { button: 0, clientX: 120, clientY: 120 });
    view.unmount();
    expect(useEditorStore.getState().isSliderDragging).toBe(false);
  });

  it("leaves recipes unchanged when no photograph is selected", () => {
    mount();
    openGroups();
    act(() => useEditorStore.setState({ selectedImage: null }));
    const before = useEditorStore.getState().adjustments;
    fireEvent.doubleClick(
      screen.getByText("Brightness", { selector: "label" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Reset Effects" }));
    expect(useEditorStore.getState().adjustments).toBe(before);
  });
});

describe("Base characteristics curve (Capture One)", () => {
  it("chooses the curve for RAW photographs and resets it", () => {
    mount();
    openGroups();
    const curve = screen.getByRole("combobox", { name: "Curve" }) as HTMLSelectElement;
    expect(curve.value).toBe("standard");
    fireEvent.change(curve, { target: { value: "highContrast" } });
    expect(useEditorStore.getState().adjustments.baseCurve).toBe("highContrast");
    fireEvent.click(screen.getByRole("button", { name: "Reset Base characteristics" }));
    expect(useEditorStore.getState().adjustments.baseCurve).toBe("standard");
  });

  it("is not offered for non-RAW photographs", () => {
    useEditorStore.setState((state) => ({
      selectedImage: { ...state.selectedImage!, path: "/qa/photo.jpg", isRaw: false },
    }));
    mount();
    openGroups();
    expect(screen.queryByRole("combobox", { name: "Curve" })).toBeNull();
  });
});

describe("Capture One-style layer masks", () => {
  const image = { width: 6000, height: 4000 } as SelectedImage;
  const withRadialLayer = () => {
    const radial = createSubMask(Mask.Radial, image);
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.masks = [
      { ...structuredClone(INITIAL_MASK_CONTAINER), id: "layer", name: "Layer 1", subMasks: [radial] },
    ];
    useEditorStore.setState({ adjustments, activeMaskContainerId: "layer", activeMaskId: radial.id });
    useUIStore.setState({ activePanel: Panel.Masks });
    return radial;
  };
  const layer = () => useEditorStore.getState().adjustments.masks[0];

  it("sets a radial mask's feathering from the Layers panel", () => {
    const radial = withRadialLayer();
    mount();
    const input = screen.getByRole("textbox", { name: "Feather value (%)" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "20" } });
    fireEvent.blur(input);
    expect(layer().subMasks.find((m) => m.id === radial.id)?.parameters.feather).toBe(0.2);
  });

  it("draws and erases with brush components that keep the gradient editable", () => {
    const radial = withRadialLayer();
    mount();
    fireEvent.click(screen.getByRole("button", { name: /Draw/ }));
    const additive = layer().subMasks.find((m) => m.type === Mask.Brush && m.mode === SubMaskMode.Additive);
    expect(additive).toBeTruthy();
    expect(useEditorStore.getState().activeMaskId).toBe(additive!.id);
    fireEvent.click(screen.getByRole("button", { name: /Erase/ }));
    const subtractive = layer().subMasks.find((m) => m.type === Mask.Brush && m.mode === SubMaskMode.Subtractive);
    expect(subtractive).toBeTruthy();
    expect(useEditorStore.getState().activeMaskId).toBe(subtractive!.id);
    // Drawing again reuses the existing component; the radial stays parametric.
    fireEvent.click(screen.getByRole("button", { name: /Draw/ }));
    expect(layer().subMasks).toHaveLength(3);
    expect(layer().subMasks[0]).toEqual(radial);
  });

  it("sets a layer's Defringe (Lightroom's local Defringe)", () => {
    withRadialLayer();
    useEditorStore.setState({ activeMaskId: null });
    mount();
    fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Detail" }));
    openGroups();
    edit("Defringe", 60);
    expect(layer().adjustments.defringe).toBe(60);
  });

  it("inverts a layer's mask, after which drawing removes from the inverted coverage", () => {
    withRadialLayer();
    mount();
    fireEvent.click(screen.getByRole("button", { name: /^Invert$/ }));
    expect(layer().invert).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: /Draw/ }));
    const active = layer().subMasks.find((m) => m.id === useEditorStore.getState().activeMaskId);
    expect(active?.mode).toBe(SubMaskMode.Subtractive);
  });

  it("creates radial layers on the outside when Draw Mask Inside is off", () => {
    useSettingsStore.setState({ appSettings: { radialDrawInside: true } as AppSettings });
    useUIStore.setState({ activePanel: Panel.Masks });
    render(
      <ContextMenuProvider>
        <StudioDevelop />
      </ContextMenuProvider>,
    );
    openLayersTab();
    fireEvent.contextMenu(screen.getByRole("button", { name: "New radial gradient layer" }));
    fireEvent.click(screen.getByText("Draw Mask Inside"));
    expect(useSettingsStore.getState().appSettings?.radialDrawInside).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "New radial gradient layer" }));
    const added = useEditorStore.getState().adjustments.masks.at(-1)!;
    expect(added.subMasks[0].type).toBe(Mask.Radial);
    expect(added.subMasks[0].invert).toBe(true);
  });
});

describe("per-tool copy and apply (Capture One)", () => {
  it("copies and resets the colour noise filter with the Noise reduction tool, as a full copy and Reset do", () => {
    useEditorStore.setState((state) => ({
      adjustments: { ...state.adjustments, colorNoiseReduction: 40, colorNoiseVersion: 1 },
    }));
    mount();
    fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Detail" }));
    fireEvent.click(screen.getByRole("button", { name: "Copy and apply Noise reduction" }), { altKey: true });
    expect(useEditorStore.getState().toolClipboard?.values).toMatchObject({
      colorNoiseReduction: 40,
      colorNoiseVersion: 1,
    });
    fireEvent.click(screen.getByRole("button", { name: "Reset Noise reduction" }));
    expect(useEditorStore.getState().adjustments.colorNoiseVersion).toBe(COLOR_NOISE_VERSION);
  });

  it("copies chosen values, applies them to the other selected photos and here", async () => {
    const { invoke } = await import("@tauri-apps/api/core");
    const { useLibraryStore } = await import("../../store/useLibraryStore");
    useEditorStore.setState((state) => ({
      adjustments: { ...state.adjustments, exposure: 1.2, contrast: 15 },
    }));
    useLibraryStore.setState({ multiSelectedPaths: ["/qa/photo.dng", "/qa/other.dng"] });
    mount();
    // Option-click copies the whole tool at once.
    fireEvent.click(screen.getByRole("button", { name: "Copy and apply Exposure" }), { altKey: true });
    expect(useEditorStore.getState().toolClipboard).toMatchObject({
      tool: "Exposure",
      source: "/qa/photo.dng",
      values: { exposure: 1.2, contrast: 15, highlights: 0 },
    });
    fireEvent.click(screen.getByRole("button", { name: "Copy and apply Exposure" }));
    const dialog = screen.getByRole("dialog", { name: "Copy and apply Exposure" });
    fireEvent.click(screen.getByRole("checkbox", { name: "Contrast" }));
    fireEvent.click(screen.getByRole("button", { name: "Apply (1)" }));
    expect(dialog.isConnected).toBe(false);
    await act(async () => {
      await vi.runAllTimersAsync();
    });
    const applied = vi
      .mocked(invoke)
      .mock.calls.find(([name]) => name === "apply_adjustments_to_paths");
    expect(applied?.[1]).toMatchObject({ paths: ["/qa/other.dng"] });
    expect((applied?.[1] as { adjustments: object }).adjustments).not.toHaveProperty("contrast");
    expect((applied?.[1] as { adjustments: object }).adjustments).toHaveProperty("exposure", 1.2);
    // On another photo the copied values can be applied here.
    act(() =>
      useEditorStore.setState({
        selectedImage: { path: "/qa/other.dng", width: 6000, height: 4000, isRaw: true, isReady: true } as SelectedImage,
        adjustments: structuredClone(INITIAL_ADJUSTMENTS),
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Copy and apply Exposure" }));
    fireEvent.click(screen.getByRole("button", { name: "Apply here" }));
    expect(useEditorStore.getState().adjustments.exposure).toBe(1.2);
    useLibraryStore.setState({ multiSelectedPaths: [] });
  });

  it("is not offered for a layer's values", () => {
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.masks = [{ ...structuredClone(INITIAL_MASK_CONTAINER), id: "l", name: "L", subMasks: [] }];
    useEditorStore.setState({ adjustments, activeMaskContainerId: "l" });
    useUIStore.setState({ activePanel: Panel.Masks });
    mount();
    expect(screen.queryByRole("button", { name: "Copy and apply Exposure" })).toBeNull();
  });
});

describe("tool on/off (Lightroom's eye)", () => {
  it("switches one tool without touching its values or the others", () => {
    useEditorStore.setState((state) => ({
      adjustments: { ...state.adjustments, temperature: 20, saturation: 30 },
    }));
    mount();
    const eye = screen.getByRole("button", { name: "White balance on" });
    expect(eye.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(eye);
    const { adjustments } = useEditorStore.getState();
    expect(adjustments.sectionVisibility.whiteBalance).toBe(false);
    expect(adjustments.sectionVisibility.color).toBe(true);
    expect(adjustments.temperature).toBe(20);
    expect(screen.getByRole("button", { name: "White balance on" }).getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(screen.getByRole("button", { name: "White balance on" }));
    expect(useEditorStore.getState().adjustments.sectionVisibility.whiteBalance).toBe(true);
  });
});

describe("layers on top of the Adjustments tab (Capture One)", () => {
  it("selecting a layer keeps the tab and the tools then edit the layer", () => {
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.masks = [
      { ...structuredClone(INITIAL_MASK_CONTAINER), id: "sky", name: "Sky", subMasks: [] },
    ];
    useEditorStore.setState({ adjustments });
    mount();
    expect(screen.getByRole("button", { name: "Adjustments" }).getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Select Sky" }));
    expect(useUIStore.getState().activePanel).toBe(Panel.Masks);
    expect(screen.getByRole("button", { name: "Adjustments" }).getAttribute("aria-pressed")).toBe("true");
    openGroups();
    edit("Exposure", 0.7);
    const state = useEditorStore.getState().adjustments;
    expect(state.masks[0].adjustments.exposure).toBe(0.7);
    expect(state.exposure).toBe(INITIAL_ADJUSTMENTS.exposure);
    // Effects belong to the photograph only.
    expect(screen.queryByText("Effects")).toBeNull();
  });
});

describe("layer order (Photoshop)", () => {
  const withLayers = () => {
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.masks = ["a", "b", "c"].map((id) => ({
      ...structuredClone(INITIAL_MASK_CONTAINER),
      id,
      name: `Layer ${id}`,
      subMasks: [],
    }));
    useEditorStore.setState({ adjustments });
    useUIStore.setState({ activePanel: Panel.Masks });
    render(
      <ContextMenuProvider>
        <StudioDevelop />
      </ContextMenuProvider>,
    );
  };
  const order = () => useEditorStore.getState().adjustments.masks.map((m) => m.id);
  const rows = () =>
    [...document.querySelectorAll<HTMLElement>("[data-layer-row]")].map((row) => row.dataset.layerRow);

  it("lists the last applied layer on top and Background at the bottom", () => {
    withLayers();
    expect(rows()).toEqual(["c", "b", "a", "background"]);
  });

  it("moves a layer from its context menu", () => {
    withLayers();
    fireEvent.contextMenu(screen.getByRole("button", { name: "Select Layer a" }));
    fireEvent.click(screen.getByText("Move Layer Up"));
    expect(order()).toEqual(["b", "a", "c"]);
    expect(rows()).toEqual(["c", "a", "b", "background"]);
  });

  it("drags a layer onto Background to apply it first, without selecting it", () => {
    withLayers();
    const background = document.querySelector<HTMLElement>('[data-layer-row="background"]')!;
    document.elementFromPoint = vi.fn(() => background);
    const select = screen.getByRole("button", { name: "Select Layer c" });
    fireEvent.pointerDown(select, { clientX: 10, clientY: 10 });
    act(() => {
      document.dispatchEvent(new MouseEvent("pointermove", { clientX: 10, clientY: 60 }));
    });
    expect(document.querySelector(".studio-drag-ghost")?.textContent).toBe("Layer c");
    act(() => {
      document.dispatchEvent(new MouseEvent("pointerup", { clientX: 10, clientY: 60 }));
    });
    fireEvent.click(select);
    expect(order()).toEqual(["c", "a", "b"]);
    expect(useEditorStore.getState().activeMaskContainerId).not.toBe("c");
    expect(document.querySelector(".studio-drag-ghost")).toBeNull();
  });
});

describe("selections, ranges and components (Lightroom's mask menu)", () => {
  const withMenu = () => {
    render(
      <ContextMenuProvider>
        <StudioDevelop />
      </ContextMenuProvider>,
    );
    openLayersTab();
  };
  const last = () => useEditorStore.getState().adjustments.masks.at(-1)!;
  beforeEach(() => useUIStore.setState({ activePanel: Panel.Masks }));
  /** A new layer: from the grid while there are none, then from the "+". */
  const newLayer = (tile: string, menuItem: string) => {
    const button = screen.queryByRole("button", { name: tile });
    if (button) return fireEvent.click(button);
    fireEvent.click(screen.getByRole("button", { name: "New layer" }));
    fireEvent.click(screen.getAllByRole("menuitem", { name: menuItem }).at(-1)!);
  };

  it("creates automatic selections and ranges as new layers", () => {
    withMenu();
    const kinds: [string, string, string, boolean][] = [
      ["subject", "Subject", Mask.AiForeground, false],
      ["sky", "Sky", Mask.AiSky, false],
      ["background", "Background", Mask.AiForeground, true],
      ["object", "Object", Mask.AiSubject, false],
      ["color range", "Color range", Mask.Color, false],
      ["depth range", "Depth range", Mask.AiDepth, false],
    ];
    for (const [label, item, type, invert] of kinds) {
      newLayer(`New ${label} layer`, item);
      expect(last().subMasks[0]).toMatchObject({ type, invert });
      expect(useEditorStore.getState().activeMaskId).toBe(last().subMasks[0].id);
    }
    // The automatic ones start without data, which the editor then computes.
    expect(last().subMasks[0].parameters.maskDataBase64).toBeNull();
    // A luminance range is the whole photo limited to a luma range.
    newLayer("New luminance range layer", "Luminance range");
    expect(last().subMasks.map((m) => m.type)).toEqual([Mask.All, Mask.LumaRange]);
  });

  it("makes a People layer of one person's parts", async () => {
    const { invoke } = await import("@tauri-apps/api/core");
    const person = (index: number, hasFeatures: boolean) => ({
      index,
      face: [0.1 + index * 0.5, 0.1, 0.1, 0.1],
      thumbnail: "data:image/jpeg;base64,",
      hasFeatures,
    });
    vi.mocked(invoke).mockImplementation(
      (name: string) =>
        Promise.resolve(
          name === "detect_people" ? [person(0, true), person(1, false)] : name === "load_metadata" ? {} : undefined,
        ) as never,
    );
    withMenu();
    fireEvent.click(screen.getByRole("button", { name: "New people layer" }));
    expect(last().subMasks[0]).toMatchObject({
      type: Mask.AiPeople,
      parameters: { parts: ["entirePerson"], person: null, maskDataBase64: null },
    });
    await act(async () => {
      await vi.runAllTimersAsync();
    });
    expect(vi.mocked(invoke).mock.calls.some(([name]) => name === "detect_people")).toBe(true);
    fireEvent.click(screen.getByRole("radio", { name: "Person 2" }));
    expect(last().subMasks[0].parameters).toMatchObject({ person: 1, maskDataBase64: null });
    // A face seen in profile has no lips to select.
    expect((screen.getByRole("checkbox", { name: "Lips" }) as HTMLInputElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: "Face skin" }));
    expect(last().subMasks[0].parameters.parts).toEqual(["faceSkin"]);
    vi.mocked(invoke).mockImplementation(
      (name: string) => Promise.resolve(name === "load_metadata" ? {} : undefined) as never,
    );
  });

  it("adds, subtracts and intersects components and changes their mode", () => {
    withMenu();
    fireEvent.click(screen.getByRole("button", { name: "New sky layer" }));
    fireEvent.click(screen.getByRole("button", { name: "Subtract" }));
    fireEvent.click(screen.getAllByRole("menuitem", { name: "Object" }).at(-1)!);
    expect(last().subMasks.map((m) => [m.type, m.mode])).toEqual([
      [Mask.AiSky, SubMaskMode.Additive],
      [Mask.AiSubject, SubMaskMode.Subtractive],
    ]);
    expect(screen.getByRole("note").textContent).toMatch(/box/);
    // The mode glyph opens Add / Subtract / Intersect; the first component has none.
    expect(screen.queryByRole("button", { name: /^Mode of Sky/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Mode of Object: Subtract" }));
    fireEvent.click(screen.getAllByRole("menuitem", { name: "Intersect" }).at(-1)!);
    expect(last().subMasks[1].mode).toBe(SubMaskMode.Intersect);
    expect(screen.getByRole("button", { name: "Mode of Object: Intersect" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Invert Object" }));
    expect(last().subMasks[1].invert).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Delete Object" }));
    expect(last().subMasks).toHaveLength(1);
  });

  it("marks the component picked in the list (or just added) for Delete, not a selected layer", () => {
    withMenu();
    fireEvent.click(screen.getByRole("button", { name: "New sky layer" }));
    expect(useEditorStore.getState().focusedMaskComponentId).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Subtract" }));
    fireEvent.click(screen.getAllByRole("menuitem", { name: "Object" }).at(-1)!);
    const [sky, object] = last().subMasks;
    expect(useEditorStore.getState().focusedMaskComponentId).toBe(object.id);
    fireEvent.click(screen.getByRole("button", { name: "Select Sky" }));
    expect(useEditorStore.getState()).toMatchObject({ activeMaskId: sky.id, focusedMaskComponentId: sky.id });
    fireEvent.click(screen.getByRole("button", { name: `Select ${last().name}` }));
    expect(useEditorStore.getState().focusedMaskComponentId).toBeNull();
  });

  it("sets a depth range's limits and a colour range's refinement", () => {
    withMenu();
    fireEvent.click(screen.getByRole("button", { name: "New depth range layer" }));
    for (const [name, value] of [["Far limit value (%)", "30"], ["Near limit value (%)", "80"]]) {
      const input = screen.getByRole("textbox", { name });
      fireEvent.focus(input);
      fireEvent.change(input, { target: { value } });
      fireEvent.blur(input);
    }
    expect(last().subMasks[0].parameters).toMatchObject({ minDepth: 30, maxDepth: 80 });
    newLayer("New color range layer", "Color range");
    edit("Colour range refine", 45);
    expect(last().subMasks[0].parameters).toMatchObject({ refine: 45 });
    expect(last().subMasks[0].parameters.tolerance).toBeUndefined();
  });

  it("renames, duplicates and duplicates inverted from the layer menu", () => {
    withMenu();
    fireEvent.click(screen.getByRole("button", { name: "New sky layer" }));
    const sky = last();
    fireEvent.contextMenu(screen.getByRole("button", { name: `Select ${sky.name}` }));
    fireEvent.click(screen.getAllByRole("menuitem", { name: /Duplicate and Invert/ }).at(-1)!);
    const copy = last();
    expect(copy.id).not.toBe(sky.id);
    expect(copy.invert).toBe(!sky.invert);
    expect(copy.subMasks[0].id).not.toBe(sky.subMasks[0].id);
    fireEvent.contextMenu(screen.getByRole("button", { name: `Select ${copy.name}` }));
    fireEvent.click(screen.getAllByRole("menuitem", { name: /Rename Layer/ }).at(-1)!);
    const input = screen.getByRole("textbox", { name: `Name of ${copy.name}` });
    fireEvent.change(input, { target: { value: "Foreground" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(last().name).toBe("Foreground");
  });
});

describe("layer panel (Capture One's list, Lightroom's components)", () => {
  const withMenu = () => {
    render(
      <ContextMenuProvider>
        <StudioDevelop />
      </ContextMenuProvider>,
    );
    openLayersTab();
  };
  const image = { width: 6000, height: 4000 } as SelectedImage;
  beforeEach(() => useUIStore.setState({ activePanel: Panel.Masks }));
  afterEach(() => useProcessStore.setState({ activeAiTasks: {} }));

  it("offers every kind as a grid of tiles while the photo has no layers", () => {
    withMenu();
    const grid = screen.getByRole("group", { name: "New layer" });
    const groups = within(grid).getAllByRole("group");
    expect(groups.map((g) => g.querySelector("p")?.textContent)).toEqual(["Draw", "Select", "Range"]);
    expect(groups.map((g) => within(g).getAllByRole("button").length)).toEqual([3, 5, 3]);
    // Each tile shows an icon above a short name.
    expect(within(groups[2]).getByRole("button", { name: "New depth range layer" }).textContent).toBe("Depth");
    expect(within(grid).getAllByRole("button").every((b) => b.querySelector("svg"))).toBe(true);
    // No list yet: not even Background.
    expect(document.querySelector("[data-layer-row]")).toBeNull();
    fireEvent.click(within(groups[0]).getByRole("button", { name: "New brush layer" }));
    const layer = useEditorStore.getState().adjustments.masks[0];
    expect(layer.subMasks[0].type).toBe(Mask.Brush);
    // Once a layer exists the list replaces the grid.
    expect(screen.queryByRole("group", { name: "New layer" })).toBeNull();
    expect([...document.querySelectorAll<HTMLElement>("[data-layer-row]")].map((r) => r.dataset.layerRow)).toEqual([
      layer.id,
      "background",
    ]);
  });

  it("adds a layer of any kind from the grouped + menu", () => {
    withMenu();
    fireEvent.click(screen.getByRole("button", { name: "New sky layer" }));
    fireEvent.click(screen.getByRole("button", { name: "New layer" }));
    const menu = screen.getAllByRole("menu").at(-1)!;
    expect([...menu.querySelectorAll(".rawtonic-menu-group")].map((h) => h.textContent)).toEqual([
      "Draw",
      "Select",
      "Range",
    ]);
    const items = within(menu).getAllByRole("menuitem");
    expect(items.map((i) => i.textContent)).toEqual([
      "Brush",
      "Linear",
      "Radial",
      "Subject",
      "Sky",
      "Background",
      "Object",
      "People",
      "Color range",
      "Luminance range",
      "Depth range",
    ]);
    expect(items.every((i) => i.querySelector("svg"))).toBe(true);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Radial" }));
    const masks = useEditorStore.getState().adjustments.masks;
    expect(masks.map((m) => m.subMasks[0].type)).toEqual([Mask.AiSky, Mask.Radial]);
    expect(useEditorStore.getState().activeMaskContainerId).toBe(masks[1].id);
  });

  it("creates the common layers in one click from the quick row, as the + menu does", () => {
    withMenu();
    fireEvent.click(screen.getByRole("button", { name: "New sky layer" }));
    const quick = screen.getByRole("toolbar", { name: "Quick new layer" });
    const buttons = within(quick).getAllByRole("button");
    expect(buttons.map((b) => b.getAttribute("aria-label"))).toEqual([
      "New subject layer",
      "New sky layer",
      "New background layer",
      "New people layer",
      "New brush layer",
      "New linear gradient layer",
      "New radial gradient layer",
    ]);
    expect(buttons.every((b) => b.querySelector("svg") && b.title)).toBe(true);
    // Without its id, a component is the same whichever way it was made.
    const shape = (index: number) =>
      useEditorStore.getState().adjustments.masks[index].subMasks.map(({ id: _, ...rest }) => rest);
    const menuItems = ["Subject", "Sky", "Background", "People", "Brush", "Linear", "Radial"];
    menuItems.forEach((item, i) => {
      const before = useEditorStore.getState().adjustments.masks.length;
      fireEvent.click(within(screen.getByRole("toolbar", { name: "Quick new layer" })).getAllByRole("button")[i]);
      const masks = useEditorStore.getState().adjustments.masks;
      expect(masks).toHaveLength(before + 1);
      expect(useEditorStore.getState().activeMaskContainerId).toBe(masks.at(-1)!.id);
      fireEvent.click(screen.getByRole("button", { name: "New layer" }));
      fireEvent.click(within(screen.getAllByRole("menu").at(-1)!).getByRole("menuitem", { name: item }));
      expect(shape(before)).toEqual(shape(before + 1));
    });
  });

  it("disables the quick row, as the + menu, until the photo is ready", () => {
    withMenu();
    fireEvent.click(screen.getByRole("button", { name: "New sky layer" }));
    act(() =>
      useEditorStore.setState({
        selectedImage: { ...useEditorStore.getState().selectedImage!, isReady: false },
      }),
    );
    const quick = screen.getByRole("toolbar", { name: "Quick new layer" });
    expect(within(quick).getAllByRole("button").every((b) => (b as HTMLButtonElement).disabled)).toBe(true);
    expect((screen.getByRole("button", { name: "New layer" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(within(quick).getByRole("button", { name: "New subject layer" }));
    expect(useEditorStore.getState().adjustments.masks).toHaveLength(1);
  });

  it("nests the selected layer's components under its row, marking busy ones", () => {
    const sky = createSubMask(Mask.AiSky, image);
    const subject = { ...createSubMask(Mask.AiForeground, image), mode: SubMaskMode.Subtractive };
    const brush = createSubMask(Mask.Brush, image);
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.masks = [
      { ...structuredClone(INITIAL_MASK_CONTAINER), id: "a", name: "Layer 1", subMasks: [brush] },
      { ...structuredClone(INITIAL_MASK_CONTAINER), id: "b", name: "Layer 2", subMasks: [sky, subject] },
    ];
    useEditorStore.setState({ adjustments, activeMaskContainerId: "b", activeMaskId: sky.id });
    useProcessStore.setState({ activeAiTasks: { [subject.id]: { id: subject.id, type: "subject", count: 1 } as never } });
    withMenu();
    const list = screen.getByRole("list", { name: "Layers" });
    const components = within(list).getByRole("group", { name: "Mask components" });
    const rowB = list.querySelector('[data-layer-row="b"]')!;
    const rowA = list.querySelector('[data-layer-row="a"]')!;
    // Directly under the selected (top) layer, before the next layer.
    expect(rowB.nextElementSibling).toBe(components);
    expect(components.nextElementSibling).toBe(rowA);
    expect(within(components).getAllByRole("listitem").map((r) => r.textContent)).toEqual([
      "Sky",
      "Subject· computing…",
    ]);
    expect(screen.getByRole("button", { name: "Mode of Subject: Subtract" })).toBeTruthy();
    // Add / Subtract / Intersect sit on one line under the components.
    const combine = within(components).getByRole("group", { name: "Combine with the mask" });
    expect(within(combine).getAllByRole("button").map((b) => b.textContent)).toEqual(["Add", "Subtract", "Intersect"]);
    // The settings follow below the list: the component's, then the layer's.
    expect(within(list).queryByRole("slider", { name: "Grow" })).toBeNull();
    expect(screen.getByRole("slider", { name: "Grow" })).toBeTruthy();
    expect(screen.getByRole("slider", { name: "Opacity" })).toBeTruthy();
    // Selecting the other layer moves the components under it.
    fireEvent.click(screen.getByRole("button", { name: "Select Layer 1" }));
    const moved = rowA.nextElementSibling as HTMLElement;
    expect(moved.getAttribute("aria-label")).toBe("Mask components");
    expect(within(moved).getAllByRole("listitem")).toHaveLength(1);
  });

  it("renames a layer by double-clicking its name and deletes it from its menu", () => {
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.masks = [{ ...structuredClone(INITIAL_MASK_CONTAINER), id: "a", name: "Layer 1", subMasks: [] }];
    useEditorStore.setState({ adjustments });
    withMenu();
    fireEvent.doubleClick(screen.getByRole("button", { name: "Select Layer 1" }));
    const input = screen.getByRole("textbox", { name: "Name of Layer 1" });
    fireEvent.change(input, { target: { value: "Sky warm" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(useEditorStore.getState().adjustments.masks[0].name).toBe("Sky warm");
    fireEvent.contextMenu(screen.getByRole("button", { name: "Select Sky warm" }));
    fireEvent.click(screen.getAllByRole("menuitem", { name: "Delete Sky warm" }).at(-1)!);
    expect(useEditorStore.getState().adjustments.masks).toHaveLength(0);
    expect(screen.getByRole("group", { name: "New layer" })).toBeTruthy();
  });
});

describe("face retouch (Capture One's Retouch Faces)", () => {
  const face = (index: number) => ({
    id: `face-${index + 1}`,
    crop: [2, 0, 100 + index * 500, 0, 2, 100],
    size: 768,
    eyeDistance: 120,
    skin: "data:image/png;base64,",
    underEyes: null,
    blemishes: [{ id: 1, x: 300, y: 300, r: 6, score: 0.9 }],
    kept: [],
    settings: {},
    enabled: true,
    box: [200 + index * 500, 200, 300, 300],
    thumbnail: "data:image/jpeg;base64,",
  });

  it("finds faces in its tab and keeps them in the recipe once a slider moves", async () => {
    const { invoke } = await import("@tauri-apps/api/core");
    vi.mocked(invoke).mockImplementation(
      (name: string) =>
        Promise.resolve(
          name === "analyze_face_retouch"
            ? { frame: "{}", faces: [face(0), face(1)] }
            : name === "load_metadata"
              ? {}
              : undefined,
        ) as never,
    );
    mount();
    fireEvent.click(screen.getByRole("button", { name: "Retouch" }));
    expect(screen.getByText("Finding faces…")).toBeTruthy();
    await act(async () => {
      await vi.runAllTimersAsync();
    });
    const call = vi.mocked(invoke).mock.calls.find(([name]) => name === "analyze_face_retouch");
    expect(call?.[1]).toMatchObject({ jsAdjustments: { orientationSteps: 0 } });
    // Viewing the tool does not change the photo.
    expect(useEditorStore.getState().adjustments.faceRetouch).toBeNull();
    edit("Face blemishes", 40);
    const retouch = () => useEditorStore.getState().adjustments.faceRetouch!;
    expect(retouch().settings).toEqual({ blemishes: 40 });
    expect(retouch().faces.map((f) => f.id)).toEqual(["face-1", "face-2"]);
    expect(retouch().frame).toBe("{}");
    // One face gets its own value; the others keep all faces' values.
    fireEvent.click(screen.getByRole("radio", { name: "Face 2" }));
    edit("Face even skin", 70);
    expect(retouch().faces[1].settings).toEqual({ evenSkin: 70 });
    expect(retouch().settings.evenSkin).toBeUndefined();
    // Copy/apply: the same retouch on the other selected photos, the second
    // face's own values sent by its place in the photo.
    const { useLibraryStore } = await import("../../store/useLibraryStore");
    act(() => useLibraryStore.setState({ multiSelectedPaths: ["/qa/photo.dng", "/qa/other.dng"] }));
    fireEvent.click(screen.getByRole("button", { name: "Apply to 1 selected photo" }));
    const applied = vi.mocked(invoke).mock.calls.find(([name]) => name === "apply_face_retouch_to_paths");
    expect(applied?.[1]).toMatchObject({
      paths: ["/qa/other.dng"],
      settings: { blemishes: 40 },
      faceSettings: [{ settings: { evenSkin: 70 } }],
    });
    const center = (applied?.[1] as { faceSettings: { center: number[] }[] }).faceSettings[0].center;
    expect(center[0]).toBeCloseTo((700 + 150) / 6000);
    act(() => useLibraryStore.setState({ multiSelectedPaths: [] }));
    fireEvent.click(screen.getByRole("button", { name: "Use the values of all faces" }));
    expect(retouch().faces[1].settings).toEqual({});
    fireEvent.click(screen.getByRole("button", { name: "Reset Face retouch" }));
    expect(useEditorStore.getState().adjustments.faceRetouch).toBeNull();
    vi.mocked(invoke).mockImplementation(
      (name: string) => Promise.resolve(name === "load_metadata" ? {} : undefined) as never,
    );
  });
});

describe("what the tools act on (tool tabs vs. the selected layer)", () => {
  const tab = (name: string) =>
    fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name }));
  const withSky = () => {
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.masks = [{ ...structuredClone(INITIAL_MASK_CONTAINER), id: "sky", name: "Sky", subMasks: [] }];
    useEditorStore.setState({ adjustments, activeMaskContainerId: "sky" });
    useUIStore.setState({ activePanel: Panel.Masks });
  };

  it("keeps the selected layer when another tool tab is chosen", () => {
    withSky();
    mount();
    tab("Detail");
    expect(useEditorStore.getState().activeMaskContainerId).toBe("sky");
    expect(useUIStore.getState().activePanel).toBe(Panel.Masks);
    expect(screen.getByRole("status").textContent).toContain("Editing: Sky");
    // A change in that tab goes to the layer, not the photo.
    openGroups();
    edit("Defringe", 30);
    expect(useEditorStore.getState().adjustments.masks[0].adjustments.defringe).toBe(30);
    expect(useEditorStore.getState().adjustments.defringePurpleAmount).toBe(0);
    // Background is an explicit choice.
    fireEvent.click(screen.getByRole("button", { name: "Edit Background" }));
    expect(useEditorStore.getState().activeMaskContainerId).toBeNull();
    expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("says where photo-only tools are and leaves the layer for Crop", () => {
    withSky();
    mount();
    tab("Retouch");
    expect(screen.getByText(/Face retouch works on the whole photo/)).toBeTruthy();
    tab("Crop, geometry & lens");
    expect(useEditorStore.getState().activeMaskContainerId).toBeNull();
    expect(useUIStore.getState().activePanel).toBe(Panel.Crop);
  });

  it("keeps the Adjust tab short while the photo has no layers", () => {
    mount();
    expect(screen.queryByRole("group", { name: "New layer" })).toBeNull();
    expect(screen.getByText(/No layers yet/)).toBeTruthy();
    // The quick row still makes a layer in one click.
    const quick = screen.getByRole("toolbar", { name: "Quick new layer" });
    fireEvent.click(within(quick).getByRole("button", { name: "New subject layer" }));
    expect(useEditorStore.getState().adjustments.masks[0].subMasks[0].type).toBe(Mask.AiForeground);
  });
});

describe("tool tabs", () => {
  it("shows every tool in one tab only (Snapshots and History in all of them)", () => {
    mount();
    const seen = new Map<string, string>();
    const twice: string[] = [];
    for (const name of ["Adjustments", "Color", "Detail", "Crop, geometry & lens", "Retouch"]) {
      fireEvent.click(within(screen.getByRole("navigation")).getByRole("button", { name }));
      for (const group of document.querySelectorAll<HTMLElement>("[data-tool-title]")) {
        const tool = group.dataset.toolTitle!;
        if (tool === "History" || tool === "Snapshots") continue;
        if (seen.has(tool)) twice.push(`${tool}: ${seen.get(tool)} + ${name}`);
        else seen.set(tool, name);
      }
    }
    expect(twice).toEqual([]);
    expect(seen.get("Exposure")).toBe("Adjustments");
    expect(seen.get("Tone curve")).toBe("Adjustments");
    expect(seen.get("Effects")).toBe("Adjustments");
    expect(seen.get("Color mixer")).toBe("Color");
    expect(seen.get("Lens correction")).toBe("Crop, geometry & lens");
  });
});


it("anchors keyboard copy at its trigger and restores focus after closing", () => {
  mount();
  const trigger = screen.getByRole("button", { name: "Copy and apply Exposure" });
  trigger.focus();
  vi.spyOn(trigger, "getBoundingClientRect").mockReturnValue({ left: 400, bottom: 300 } as DOMRect);
  fireEvent.click(trigger, { detail: 0 });
  const panel = screen.getByRole("dialog", { name: "Copy and apply Exposure" });
  expect(panel.style.left).toBe("400px");
  expect(panel.contains(document.activeElement)).toBe(true);
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(document.activeElement).toBe(trigger);
});
