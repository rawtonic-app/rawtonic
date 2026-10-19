import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import StudioMaskComponents from "./StudioMaskComponents";
import { useStudioTools } from "./useStudioTools";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, type Adjustments } from "../../utils/adjustments";
import { createSubMask } from "../../utils/maskUtils";
import { erasesFromLayer } from "../../utils/maskKinds";
import { Mask, SubMaskMode } from "../panel/right/Masks";
import { Panel } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
const showContextMenu = vi.hoisted(() => vi.fn());
vi.mock("../../context/ContextMenuContext", () => ({ useContextMenu: () => ({ showContextMenu }) }));
const setAdjustments = vi.fn((update: Adjustments | ((prev: Adjustments) => Adjustments)) => {
  const prev = useEditorStore.getState().adjustments;
  useEditorStore.setState({ adjustments: typeof update === "function" ? update(prev) : update });
});
vi.mock("../../hooks/useEditorActions", () => ({ useEditorActions: () => ({ setAdjustments }) }));

const size = { width: 1000, height: 800 };
const layerOf = () => useEditorStore.getState().adjustments.masks[0];

function setLayer(invert: boolean) {
  const linear = createSubMask(Mask.Linear, size);
  useEditorStore.setState({
    selectedImage: { path: "/a.arw", isReady: true, ...size } as never,
    adjustments: {
      ...INITIAL_ADJUSTMENTS,
      masks: [{ ...structuredClone(INITIAL_MASK_CONTAINER), id: "l1", name: "Layer 1", invert, subMasks: [linear] }],
    },
    activeMaskContainerId: "l1",
    activeMaskId: linear.id,
  });
}

/** Paints on the layer as the viewer's brush (`draw`) or eraser does. */
function paint(draw: boolean) {
  let tools: ReturnType<typeof useStudioTools> | undefined;
  const Probe = () => {
    tools = useStudioTools();
    return null;
  };
  const { unmount } = render(<Probe />);
  act(() => tools!.paintOnLayer(layerOf(), draw));
  unmount();
}

const labels = () =>
  Array.from(document.querySelectorAll(".studio-mask-component-label")).map((e) => e.textContent);

beforeEach(() => {
  setLayer(false);
  useUIStore.setState({ activePanel: Panel.Masks });
});
afterEach(cleanup);

describe("eraser components", () => {
  it("read Erase on a plain layer (a subtracting brush)", () => {
    paint(false);
    expect(layerOf().subMasks[1].mode).toBe(SubMaskMode.Subtractive);
    render(<StudioMaskComponents />);
    expect(labels()).toEqual(["Linear", "Erase"]);
  });

  it("read Erase on an inverted layer, where the eraser adds", () => {
    setLayer(true);
    paint(false);
    expect(layerOf().subMasks[1].mode).toBe(SubMaskMode.Additive);
    render(<StudioMaskComponents />);
    expect(labels()).toEqual(["Linear", "Erase"]);
    expect(screen.getByLabelText("Select Erase")).toBeTruthy();
  });

  it("leave drawing brushes reading Brush, inverted or not", () => {
    paint(true);
    render(<StudioMaskComponents />);
    expect(labels()).toEqual(["Linear", "Brush"]);
    cleanup();
    setLayer(true);
    paint(true);
    expect(layerOf().subMasks[1].mode).toBe(SubMaskMode.Subtractive);
    render(<StudioMaskComponents />);
    expect(labels()).toEqual(["Linear", "Brush"]);
  });

  it("follow the layer's invert and only apply to brushes", () => {
    const brush = createSubMask(Mask.Brush, size, SubMaskMode.Subtractive);
    expect(erasesFromLayer(brush, false)).toBe(true);
    expect(erasesFromLayer(brush, true)).toBe(false);
    expect(erasesFromLayer({ ...brush, mode: SubMaskMode.Additive }, true)).toBe(true);
    expect(erasesFromLayer({ ...brush, mode: SubMaskMode.Intersect }, true)).toBe(false);
    expect(erasesFromLayer(createSubMask(Mask.Radial, size, SubMaskMode.Subtractive), false)).toBe(false);
  });
});

describe("the combine menus", () => {
  /** Picks `kind` from the menu the combine button `title` opens. */
  function pick(title: string, kind: string) {
    showContextMenu.mockClear();
    render(<StudioMaskComponents />);
    act(() => screen.getByTitle(title).click());
    const items = showContextMenu.mock.calls.at(-1)?.[2] as { label?: string; onClick?: () => void }[];
    act(() => items.find((item) => item.label === kind)!.onClick!());
  }

  it("intersect the layer with a new brush instead of painting on it", () => {
    pick("Keep only where this and the mask overlap", "Brush");
    const added = layerOf().subMasks[1];
    expect(added).toMatchObject({ type: Mask.Brush, mode: SubMaskMode.Intersect });
    expect(useEditorStore.getState().activeMaskId).toBe(added.id);
  });

  it("still paint and erase with Add and Subtract", () => {
    pick("Add to this layer's mask", "Brush");
    expect(layerOf().subMasks[1]).toMatchObject({ type: Mask.Brush, mode: SubMaskMode.Additive });
    cleanup();
    setLayer(false);
    pick("Subtract from this layer's mask", "Brush");
    expect(layerOf().subMasks[1]).toMatchObject({ type: Mask.Brush, mode: SubMaskMode.Subtractive });
  });
});
