import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import StudioLayerControls from "./StudioLayerControls";
import { useStudioTools } from "./useStudioTools";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, type Adjustments } from "../../utils/adjustments";
import { addComponentBeforeLumaRange, createSubMask } from "../../utils/maskUtils";
import { Mask, SubMaskMode } from "../panel/right/Masks";
import { Panel } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
const setAdjustments = vi.fn((update: Adjustments | ((prev: Adjustments) => Adjustments)) => {
  const prev = useEditorStore.getState().adjustments;
  useEditorStore.setState({ adjustments: typeof update === "function" ? update(prev) : update });
});
vi.mock("../../hooks/useEditorActions", () => ({ useEditorActions: () => ({ setAdjustments }) }));

const size = { width: 1000, height: 800 };
const layerOf = () => useEditorStore.getState().adjustments.masks[0];

beforeEach(() => {
  const linear = createSubMask(Mask.Linear, size);
  useEditorStore.setState({
    selectedImage: { path: "/a.arw", isReady: true, ...size } as never,
    adjustments: {
      ...INITIAL_ADJUSTMENTS,
      masks: [{ ...structuredClone(INITIAL_MASK_CONTAINER), id: "l1", name: "Layer 1", subMasks: [linear] }],
    },
    activeMaskContainerId: "l1",
    activeMaskId: linear.id,
  });
  useUIStore.setState({ activePanel: Panel.Masks });
});
afterEach(cleanup);

describe("luma range", () => {
  it("is created as an intersecting component with the full range", () => {
    const range = createSubMask(Mask.LumaRange, size);
    expect(range.mode).toBe(SubMaskMode.Intersect);
    expect(range.parameters).toEqual({ rangeLow: 0, rangeHigh: 100, falloff: 20 });
  });

  it("stays the last component when brush components are added", () => {
    const linear = createSubMask(Mask.Linear, size);
    const range = createSubMask(Mask.LumaRange, size);
    const brush = createSubMask(Mask.Brush, size);
    expect(addComponentBeforeLumaRange([linear, range], brush).map((m) => m.type)).toEqual([
      Mask.Linear,
      Mask.Brush,
      Mask.LumaRange,
    ]);
    expect(addComponentBeforeLumaRange([linear], brush).map((m) => m.type)).toEqual([Mask.Linear, Mask.Brush]);
  });

  it("is added, adjusted and removed from the layer controls", () => {
    render(<StudioLayerControls />);
    fireEvent.click(screen.getByText("Limit to a luma range…"));
    expect(layerOf().subMasks.map((m) => m.type)).toEqual([Mask.Linear, Mask.LumaRange]);
    expect(screen.getByLabelText("Luma range")).toBeTruthy();
    fireEvent.click(screen.getByLabelText("Remove luma range"));
    expect(layerOf().subMasks.map((m) => m.type)).toEqual([Mask.Linear]);
  });

  it("keeps limiting what the brush draws on the layer", () => {
    useEditorStore.setState((state) => ({
      adjustments: {
        ...state.adjustments,
        masks: [{ ...layerOf(), subMasks: [...layerOf().subMasks, createSubMask(Mask.LumaRange, size)] }],
      },
    }));
    let tools: ReturnType<typeof useStudioTools> | undefined;
    const Probe = () => {
      tools = useStudioTools();
      return null;
    };
    render(<Probe />);
    tools!.paintOnLayer(layerOf(), true);
    expect(layerOf().subMasks.map((m) => m.type)).toEqual([Mask.Linear, Mask.Brush, Mask.LumaRange]);
  });
});

describe("luma range eyedropper", () => {
  const probe = () => {
    let tools: ReturnType<typeof useStudioTools> | undefined;
    const Probe = () => {
      tools = useStudioTools();
      return null;
    };
    render(<Probe />);
    return tools!;
  };

  it("is switched on and off beside the range, with a hint while on", () => {
    useEditorStore.setState({ lumaPickerLayer: null });
    render(<StudioLayerControls />);
    fireEvent.click(screen.getByText("Limit to a luma range…"));
    const picker = screen.getByLabelText("Sample tones on the photo");
    expect(picker.getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(picker);
    expect(useEditorStore.getState().lumaPickerLayer).toBe("l1");
    expect(picker.getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("note").textContent).toMatch(/Click a tone on the photo/);
    fireEvent.click(picker);
    expect(useEditorStore.getState().lumaPickerLayer).toBeNull();
  });

  it("starts with a new luminance range, as in Lightroom", () => {
    useEditorStore.setState({ lumaPickerLayer: null });
    const tools = probe();
    tools.addComponent(layerOf(), "luminance", SubMaskMode.Intersect);
    expect(useEditorStore.getState().lumaPickerLayer).toBe("l1");
    useEditorStore.setState({ lumaPickerLayer: null });
    tools.addLayer("luminance");
    const { lumaPickerLayer, activeMaskContainerId } = useEditorStore.getState();
    expect(lumaPickerLayer).toBe(activeMaskContainerId);
    expect(lumaPickerLayer).not.toBe("l1");
  });
});

describe("brush auto mask", () => {
  it("turns Auto Mask on for the strokes to come", () => {
    render(<StudioLayerControls />);
    fireEvent.click(screen.getByTitle("Paint onto this layer's mask (keeps gradients editable)"));
    const toggle = screen.getByLabelText("Auto mask") as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    fireEvent.click(toggle);
    expect(useEditorStore.getState().brushSettings?.autoMask).toBe(true);
  });
});

describe("refine edge", () => {
  it("sets the radius on the layer and moves it to the 16-bit mask", () => {
    render(<StudioLayerControls />);
    const type = (value: string) => {
      const field = screen.getByTitle(/layer's edge follows the photo's own edge/).querySelector("input[type=text]")!;
      fireEvent.focus(field);
      fireEvent.change(field, { target: { value } });
      fireEvent.blur(field);
    };
    type("40");
    expect(layerOf()).toMatchObject({ refineEdge: 40, maskVersion: 3 });
    type("0");
    expect(layerOf().refineEdge).toBeUndefined();
    expect(layerOf().maskVersion).toBe(3);
  });

  it("gives new layers the 16-bit mask and leaves older ones alone", () => {
    let tools: ReturnType<typeof useStudioTools> | undefined;
    const Probe = () => {
      tools = useStudioTools();
      return null;
    };
    render(<Probe />);
    tools!.addLayer(Mask.Radial);
    const masks = useEditorStore.getState().adjustments.masks;
    expect(masks[0].maskVersion).toBeUndefined();
    expect(masks[1].maskVersion).toBe(3);
  });
});
