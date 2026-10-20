import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import StudioLayers from "./StudioLayers";
import { useEditorStore } from "../../store/useEditorStore";
import { useProcessStore } from "../../store/useProcessStore";
import { useUIStore } from "../../store/useUIStore";
import { useMaskThumbnailStore } from "../../hooks/useMaskThumbnails";
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER } from "../../utils/adjustments";
import { createSubMask } from "../../utils/maskUtils";
import { Mask, ToolType } from "../panel/right/Masks";
import type { Adjustments } from "../../utils/adjustments";
import { Panel } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
const { showContextMenu, setAdjustments, toastError } = vi.hoisted(() => ({
  showContextMenu: vi.fn(),
  setAdjustments: vi.fn(),
  toastError: vi.fn(),
}));
vi.mock("react-toastify", () => ({ toast: { error: toastError } }));
vi.mock("../../context/ContextMenuContext", () => ({ useContextMenu: () => ({ showContextMenu }) }));
vi.mock("../../hooks/useEditorActions", () => ({ useEditorActions: () => ({ setAdjustments }) }));
// The rows only read the thumbnails (useMaskThumbnails.test.ts covers updates).
vi.mock("../../hooks/useMaskThumbnails", async (original) => ({
  ...(await original<typeof import("../../hooks/useMaskThumbnails")>()),
  useMaskThumbnailUpdates: () => undefined,
}));

const size = { width: 6000, height: 4000 };
const radial = createSubMask(Mask.Radial, size);
const subject = createSubMask(Mask.AiForeground, size);
const brush = createSubMask(Mask.Brush, size);

/** Thumbnail boxes of the rows, top row first. */
const layerThumbs = () => Array.from(document.querySelectorAll(".studio-layer .studio-mask-thumb.is-layer"));
const componentThumbs = () => Array.from(document.querySelectorAll(".studio-mask-thumb.is-component"));

beforeEach(() => {
  vi.clearAllMocks();
  useUIStore.setState({ activePanel: Panel.Masks });
  useProcessStore.setState({ activeAiTasks: {} });
  useEditorStore.setState({
    selectedImage: { path: "/a.cr3", isReady: true, ...size } as never,
    adjustments: {
      ...structuredClone(INITIAL_ADJUSTMENTS),
      masks: [
        { ...structuredClone(INITIAL_MASK_CONTAINER), id: "l1", name: "Vignette", subMasks: [radial] },
        { ...structuredClone(INITIAL_MASK_CONTAINER), id: "l2", name: "Person", subMasks: [subject, brush] },
      ],
    },
    activeMaskContainerId: "l2",
    activeMaskId: subject.id,
    focusedMaskComponentId: null,
    layerSolo: null,
  });
  useMaskThumbnailStore.setState({ path: "/a.cr3", layers: {}, components: {} });
});
afterEach(cleanup);

describe("layer list mask thumbnails", () => {
  it("shows each layer's thumbnail and the selected layer's component thumbnails", () => {
    useMaskThumbnailStore.setState({
      layers: { l1: "data:image/png;base64,l1", l2: "data:image/png;base64,l2" },
      components: { [subject.id]: "data:image/png;base64,s", [brush.id]: "data:image/png;base64,b" },
    });
    render(<StudioLayers />);
    // Top row = the layer applied last (l2), then l1; the Background row
    // keeps a plain box for alignment.
    const thumbs = layerThumbs();
    expect(thumbs.map((t) => t.querySelector("img")?.getAttribute("src"))).toEqual([
      "data:image/png;base64,l2",
      "data:image/png;base64,l1",
      undefined,
    ]);
    expect(thumbs[2].classList.contains("is-plain")).toBe(true);
    expect(componentThumbs().map((t) => t.querySelector("img")?.getAttribute("src"))).toEqual([
      "data:image/png;base64,s",
      "data:image/png;base64,b",
    ]);
    // Decorative: the row's button keeps its name.
    expect(thumbs[0].getAttribute("aria-hidden")).toBe("true");
  });

  it("shows the kind's icon until a thumbnail is made, and pulses while an AI mask computes", () => {
    useMaskThumbnailStore.setState({ layers: { l1: "data:image/png;base64,l1" } });
    useProcessStore.setState({ activeAiTasks: { [subject.id]: { id: subject.id, type: "foreground", count: 1 } } });
    render(<StudioLayers />);
    const [person, vignette] = layerThumbs();
    expect(person.querySelector("img")).toBeNull();
    expect(person.querySelector("svg")).not.toBeNull();
    expect(person.classList.contains("is-busy")).toBe(true);
    expect(vignette.classList.contains("is-busy")).toBe(false);
    const [subjectThumb, brushThumb] = componentThumbs();
    expect(subjectThumb.classList.contains("is-busy")).toBe(true);
    expect(brushThumb.classList.contains("is-busy")).toBe(false);
    expect(brushThumb.querySelector("svg")).not.toBeNull();
  });
});

type MenuItem = { label?: string; onClick?: () => void };
const menuOf = (name: string) => {
  fireEvent.contextMenu(document.querySelector(`[data-layer-row="${name}"]`)!);
  return showContextMenu.mock.lastCall![2] as MenuItem[];
};

describe("layer solo (view only)", () => {
  it("Option/Alt-click on a layer's eye shows only that layer; a plain click still hides it", () => {
    render(<StudioLayers />);
    const eye = screen.getByRole("button", { name: "Toggle Vignette" });
    fireEvent.click(eye, { altKey: true });
    expect(useEditorStore.getState().layerSolo).toEqual({ layerId: "l1", componentId: null });
    expect(setAdjustments).not.toHaveBeenCalled();
    fireEvent.click(eye, { altKey: true });
    expect(useEditorStore.getState().layerSolo).toBeNull();
    fireEvent.click(eye);
    expect(setAdjustments).toHaveBeenCalledTimes(1);
  });

  it("names the solo above the list, follows the selection and ends with Show all or the Background", () => {
    useEditorStore.setState({ layerSolo: { layerId: "l2", componentId: null } });
    render(<StudioLayers />);
    expect(screen.getByText("Showing only Person")).toBeTruthy();
    expect(document.querySelector('[data-layer-row="l2"]')!.classList.contains("is-solo")).toBe(true);
    expect(document.querySelector('[data-layer-row="l1"]')!.classList.contains("is-solo-muted")).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Select Vignette" }));
    expect(useEditorStore.getState().layerSolo).toEqual({ layerId: "l1", componentId: null });
    fireEvent.click(screen.getByText("Show all"));
    expect(useEditorStore.getState().layerSolo).toBeNull();
    useEditorStore.setState({ layerSolo: { layerId: "l2", componentId: null } });
    fireEvent.click(screen.getByText("Background"));
    expect(useEditorStore.getState().layerSolo).toBeNull();
  });

  it("Option/Alt-click on a component's eye shows only that mask", () => {
    render(<StudioLayers />);
    const [, brushEye] = Array.from(document.querySelectorAll(".studio-mask-eye"));
    fireEvent.click(brushEye, { altKey: true });
    expect(useEditorStore.getState().layerSolo).toEqual({ layerId: "l2", componentId: brush.id });
    expect(setAdjustments).not.toHaveBeenCalled();
  });

  it("offers Show only this layer with its key in the layer menu", () => {
    render(<StudioLayers />);
    const solo = menuOf("l1").find((item) => item.label?.startsWith("Show only this layer"));
    expect(solo?.label).toBe("Show only this layer (H)");
    solo!.onClick!();
    expect(useEditorStore.getState().layerSolo).toEqual({ layerId: "l1", componentId: null });
  });
});

describe("Upgrade to current Auto Mask (D6)", () => {
  const strokes = (autoMaskVersion?: number) => [
    { brushSize: 40, points: [{ x: 1, y: 1 }], tool: ToolType.Brush, autoMask: true, autoMaskVersion },
  ];
  const withStrokes = (autoMaskVersion?: number) => {
    const masks = structuredClone(useEditorStore.getState().adjustments.masks);
    masks[1].subMasks[1].parameters = { ...masks[1].subMasks[1].parameters, lines: strokes(autoMaskVersion) };
    useEditorStore.setState({ adjustments: { ...useEditorStore.getState().adjustments, masks } });
    return masks;
  };

  it("appears only on layers with older Auto Mask strokes and upgrades them in one step", () => {
    const masks = withStrokes(undefined);
    render(<StudioLayers />);
    expect(menuOf("l1").some((item) => item.label?.startsWith("Upgrade"))).toBe(false);
    const upgrade = menuOf("l2").find((item) => item.label?.startsWith("Upgrade"));
    expect(upgrade?.label).toBe("Upgrade to current Auto Mask (version 1 → 3)");
    upgrade!.onClick!();
    expect(setAdjustments).toHaveBeenCalledTimes(1);
    const update = setAdjustments.mock.calls[0][0] as (prev: Adjustments) => Adjustments;
    const next = update({ ...structuredClone(INITIAL_ADJUSTMENTS), masks });
    expect(next.masks[1].subMasks[1].parameters.lines![0].autoMaskVersion).toBe(3);
    expect(next.masks[0]).toBe(masks[0]);
  });

  it("is not offered when the strokes are on the current version", () => {
    withStrokes(3);
    render(<StudioLayers />);
    expect(menuOf("l2").some((item) => item.label?.startsWith("Upgrade"))).toBe(false);
  });
});

describe("rendered layer limit", () => {
  /** `count` visible radial layers l1… plus a hidden one ("hidden"). */
  const layers = (count: number) => {
    const masks = Array.from({ length: count }, (_, i) => ({
      ...structuredClone(INITIAL_MASK_CONTAINER),
      id: `l${i + 1}`,
      name: `Layer ${i + 1}`,
      subMasks: [createSubMask(Mask.Radial, size)],
    }));
    masks.push({ ...masks[0], id: "hidden", name: "Hidden", visible: false });
    useEditorStore.setState({ adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks } });
  };

  it("refuses Duplicate and showing a hidden layer beyond 32 with a message", () => {
    layers(32);
    render(<StudioLayers />);
    menuOf("l1").find((item) => item.label === "Duplicate Layer")!.onClick!();
    fireEvent.click(screen.getByRole("button", { name: "Toggle Hidden" }));
    expect(setAdjustments).not.toHaveBeenCalled();
    expect(toastError).toHaveBeenCalledTimes(2);
    expect(toastError.mock.calls[0][0]).toMatch(/Up to 32 visible layers/);
    // Hiding a layer stays possible, and so does duplicating a hidden one.
    fireEvent.click(screen.getByRole("button", { name: "Toggle Layer 1" }));
    menuOf("hidden").find((item) => item.label === "Duplicate Layer")!.onClick!();
    expect(setAdjustments).toHaveBeenCalledTimes(2);
  });

  it("allows them below the limit", () => {
    layers(31);
    render(<StudioLayers />);
    menuOf("l1").find((item) => item.label === "Duplicate Layer")!.onClick!();
    fireEvent.click(screen.getByRole("button", { name: "Toggle Hidden" }));
    expect(setAdjustments).toHaveBeenCalledTimes(2);
    expect(toastError).not.toHaveBeenCalled();
  });

  it("marks the layers an older recipe holds beyond the limit as not applied", () => {
    layers(34);
    render(<StudioLayers />);
    const marked = Array.from(document.querySelectorAll(".studio-layer-unrendered")).map(
      (badge) => badge.closest("[data-layer-row]")!.getAttribute("data-layer-row"),
    );
    expect(marked.sort()).toEqual(["l33", "l34"]);
  });
});
