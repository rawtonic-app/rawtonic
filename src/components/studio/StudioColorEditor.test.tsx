import "../../i18n";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import {
  newColorRange,
  type Adjustments,
  type ColorRange,
} from "../../utils/adjustments";
import { useEditorStore } from "../../store/useEditorStore";
import StudioColorEditor from "./StudioColorEditor";
import StudioColorRangeWheel, {
  chromaOf,
  radiusOf,
  rangeBand,
  wheelPoint,
} from "./StudioColorRangeWheel";

/** The wheel is drawn 224 px wide: one view-box unit (±1.12) is 100 px. */
const WHEEL_PX = 224;
const at = (hue: number, chroma: number) => {
  const p = wheelPoint(hue, chroma);
  return { clientX: (p.x + 1.12) * 100, clientY: (p.y + 1.12) * 100 };
};

let changes: Partial<Adjustments>[] = [];
const interaction = vi.fn();

function Harness({ initial }: { initial: ColorRange[] }) {
  const [values, setValues] = useState<Partial<Adjustments>>({
    colorRanges: initial,
  });
  return (
    <StudioColorEditor
      values={values}
      disabled={false}
      onInteraction={interaction}
      change={(patch) => {
        changes.push(patch);
        setValues((v) => ({ ...v, ...patch }));
      }}
    />
  );
}

const lastRanges = () => changes.at(-1)!.colorRanges as ColorRange[];

const wheelSvg = () => {
  const svg = screen
    .getByRole("group", { name: "Color range wheel" })
    .querySelector("svg")!;
  vi.spyOn(svg, "getBoundingClientRect").mockReturnValue({
    left: 0,
    top: 0,
    width: WHEEL_PX,
    height: WHEEL_PX,
  } as DOMRect);
  return svg;
};

const openTool = () => {
  if (!screen.queryByRole("tablist")) {
    fireEvent.click(screen.getByRole("button", { name: /Color editor/ }));
  }
};

beforeEach(() => {
  changes = [];
  interaction.mockReset();
  useEditorStore.setState({
    colorPickerTarget: null,
    colorPickerLayer: null,
    activeColorRangeId: null,
    colorRangeShow: null,
  });
});
afterEach(cleanup);

const skinish = () => ({
  ...newColorRange(50, 0.06, 0.65),
  hueRange: 25,
  chromaRange: 50,
  smoothness: 50,
});

describe("range wheel geometry", () => {
  it("maps chroma to the radius and back", () => {
    for (const c of [0, 0.02, 0.06, 0.15, 0.32]) {
      expect(chromaOf(radiusOf(c))).toBeCloseTo(c, 6);
    }
    // Hue 0 at the right, 90 at the top (screen y down).
    expect(wheelPoint(90, 0.32).y).toBeCloseTo(-1, 6);
  });

  it("builds the band the renderer selects", () => {
    const band = rangeBand(skinish());
    expect(band.hueLo).toBe(25);
    expect(band.hueHi).toBe(75);
    expect(band.chromaLo).toBeCloseTo(0.03, 6);
    expect(band.chromaHi).toBeCloseTo(0.06 * 2 + 0.01, 6);
    // Smoothness 50 → softness 0.55: the hue weight is full within
    // ±25 × 0.45 and zero beyond ±25 × 1.55; the chroma feather is
    // 0.5 × 0.55 × 0.1 + 0.004 on both sides of each chroma edge.
    expect(band.core!.hueLo).toBeCloseTo(50 - 11.25, 6);
    expect(band.core!.hueHi).toBeCloseTo(50 + 11.25, 6);
    expect(band.outer.hueHi).toBeCloseTo(50 + 38.75, 6);
    expect(band.core!.chromaLo).toBeCloseTo(0.03 + 0.0315, 6);
    expect(band.core!.chromaHi).toBeCloseTo(0.13 - 0.0315, 6);
    expect(band.outer.chromaHi).toBeCloseTo(0.13 + 0.0315, 6);
    // A band narrower than its feathers has no full-weight core.
    expect(rangeBand({ ...skinish(), chromaRange: 0 }).core).toBeNull();
    // A grey sample: the band of chroma 0 (0 … 0.01, feather 0.00675),
    // faded by the grey gate, so no full-weight core.
    const grey = rangeBand({ ...skinish(), chroma: 0 });
    expect(grey.outer.chromaHi).toBeCloseTo(0.01675, 6);
    expect(grey.outer.chromaLo).toBe(0.004);
    expect(grey.core).toBeNull();
    // The core starts where the grey gate is full.
    const pale = rangeBand({
      ...skinish(),
      chroma: 0.02,
      chromaRange: 100,
      smoothness: 0,
    });
    expect(pale.core!.chromaLo).toBe(0.015);
  });
});

describe("range wheel <-> sliders", () => {
  it("dragging a hue edge sets the Range hue slider", () => {
    const range = skinish();
    render(<Harness initial={[range]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole("slider", {
      name: "Range hue edge, counter-clockwise",
    });
    expect(edge.getAttribute("aria-valuenow")).toBe("25");
    fireEvent.pointerDown(edge, { button: 0, ...at(75, 0.06) });
    expect(interaction).toHaveBeenLastCalledWith(true);
    fireEvent.pointerMove(edge, at(90, 0.06));
    fireEvent.pointerUp(edge);
    expect(interaction).toHaveBeenLastCalledWith(false);
    expect(lastRanges()[0].hueRange).toBe(40);
    expect(lastRanges()[0].hue).toBe(50);
    // The slider shows the dragged width.
    const slider = screen.getByRole("slider", {
      name: "Color range hue range",
    });
    expect((slider as HTMLInputElement).value).toBe("40");
  });

  it("dragging the outer saturation edge sets the saturation range", () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole("slider", {
      name: "Range outer saturation edge",
    });
    // Outer edge at c(1 + 2r) + 0.01: 0.1 → r = (0.1 − 0.01 − 0.06) / 0.12.
    fireEvent.pointerDown(edge, { button: 0, ...at(50, 0.13) });
    fireEvent.pointerMove(edge, at(50, 0.1));
    fireEvent.pointerUp(edge);
    expect(lastRanges()[0].chromaRange).toBe(25);
    expect(
      (
        screen.getByRole("slider", {
          name: "Color range saturation range",
        }) as HTMLInputElement
      ).value,
    ).toBe("25");
  });

  it("keeps a saturated range's out-of-wheel width until the edge is dragged", () => {
    // Outer edge 0.2 × 2.4 + 0.01 = 0.49: beyond the rim, so the handle sits
    // on the rim; pressing and moving it there must not change the range.
    const range = { ...newColorRange(30, 0.2, 0.6), chromaRange: 70 };
    render(<Harness initial={[range]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole("slider", {
      name: "Range outer saturation edge",
    });
    fireEvent.pointerDown(edge, { button: 0, ...at(30, 0.32) });
    fireEvent.pointerMove(edge, at(30, 0.32));
    fireEvent.pointerUp(edge);
    expect(changes).toHaveLength(0);
    // Dragged inwards it narrows from where it was, by the pointer's way.
    fireEvent.pointerDown(edge, { button: 0, ...at(30, 0.32) });
    fireEvent.pointerMove(edge, at(30, 0.26));
    fireEvent.pointerUp(edge);
    const width = lastRanges()[0].chromaRange;
    // Edge 0.49 − 0.06 = 0.43 → (0.43 − 0.01 − 0.2) / 0.4 = 55.
    expect(width).toBe(55);
  });

  it("draws the edge handles over the colour handle, so they stay reachable", () => {
    // Saturated blue: the outer edge clips to the rim right beside the
    // colour; SVG paints later elements on top, so the edges come last.
    render(<Harness initial={[newColorRange(264, 0.313, 0.45)]} />);
    openTool();
    const colour = screen.getByRole("slider", { name: "Range color" });
    for (const name of [
      "Range outer saturation edge",
      "Range inner saturation edge",
      "Range hue edge, clockwise",
    ]) {
      const edge = screen.getByRole("slider", { name });
      expect(
        colour.compareDocumentPosition(edge) & Node.DOCUMENT_POSITION_FOLLOWING,
      ).toBeTruthy();
    }
  });

  it("selecting another range mid-drag ends the drag without editing either", () => {
    const a = skinish();
    const b = { ...newColorRange(220, 0.08, 0.5), hueRange: 25 };
    render(<Harness initial={[a, b]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole("slider", {
      name: "Range hue edge, counter-clockwise",
    });
    fireEvent.pointerDown(edge, { button: 0, ...at(75, 0.06) });
    fireEvent.pointerMove(edge, at(90, 0.06));
    act(() => useEditorStore.setState({ activeColorRangeId: b.id }));
    expect(interaction).toHaveBeenLastCalledWith(false);
    fireEvent.pointerMove(edge, at(100, 0.06));
    fireEvent.pointerUp(edge);
    // Nothing was written: the pending frame of range A was dropped.
    expect(changes).toHaveLength(0);
    expect(
      screen
        .getByRole("slider", { name: "Range hue edge, counter-clockwise" })
        .getAttribute("aria-valuenow"),
    ).toBe("25");
  });

  it("another photograph mid-drag ends the drag (copied ranges share ids)", () => {
    useEditorStore.setState({
      selectedImage: { path: "/a.cr3" } as never,
    });
    render(<Harness initial={[skinish()]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole("slider", {
      name: "Range hue edge, counter-clockwise",
    });
    fireEvent.pointerDown(edge, { button: 0, ...at(75, 0.06) });
    fireEvent.pointerMove(edge, at(90, 0.06));
    act(() =>
      useEditorStore.setState({ selectedImage: { path: "/b.cr3" } as never }),
    );
    fireEvent.pointerMove(edge, at(100, 0.06));
    fireEvent.pointerUp(edge);
    expect(changes).toHaveLength(0);
    useEditorStore.setState({ selectedImage: null });
  });

  it("a near-grey range keeps its width on a press without movement", () => {
    const range = { ...newColorRange(50, 0.004, 0.6), chromaRange: 50 };
    render(<Harness initial={[range]} />);
    openTool();
    wheelSvg();
    const edge = screen.getByRole("slider", {
      name: "Range outer saturation edge",
    });
    fireEvent.pointerDown(edge, { button: 0, ...at(50, 0.018) });
    fireEvent.pointerMove(edge, at(50, 0.018));
    fireEvent.pointerUp(edge);
    expect(changes).toHaveLength(0);
  });

  it("disabled mid-drag shows the recipe's range again", () => {
    const range = skinish();
    const ranges = [range];
    const props = {
      ranges,
      active: range,
      onChange: vi.fn(),
      onInteraction: vi.fn(),
    };
    const { rerender } = render(<StudioColorRangeWheel {...props} />);
    wheelSvg();
    const edge = () =>
      screen.getByRole("slider", { name: "Range hue edge, counter-clockwise" });
    fireEvent.pointerDown(edge(), { button: 0, ...at(75, 0.06) });
    fireEvent.pointerMove(edge(), at(90, 0.06));
    expect(edge().getAttribute("aria-valuenow")).toBe("40");
    rerender(<StudioColorRangeWheel {...props} disabled />);
    expect(props.onInteraction).toHaveBeenLastCalledWith(false);
    rerender(<StudioColorRangeWheel {...props} />);
    expect(edge().getAttribute("aria-valuenow")).toBe("25");
    expect(props.onChange).not.toHaveBeenCalled();
  });

  it("follows the sliders: a new hue range moves the edge handles", () => {
    const range = skinish();
    const { rerender } = render(<Harness initial={[range]} />);
    openTool();
    const edge = () =>
      screen.getByRole("slider", { name: "Range hue edge, clockwise" });
    const before = Number(edge().getAttribute("cy"));
    const slider = screen.getByRole("slider", {
      name: "Color range hue range",
    });
    fireEvent.keyDown(slider, { key: "ArrowRight" });
    rerender(<Harness initial={[range]} />);
    expect(lastRanges()[0].hueRange).toBe(26);
    expect(edge().getAttribute("aria-valuenow")).toBe("26");
    // The clockwise edge (hue 50 − 26) sits lower on the wheel than before.
    expect(Number(edge().getAttribute("cy"))).toBeGreaterThan(before);
  });

  it("steps a handle with the arrow keys", () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    const edge = screen.getByRole("slider", {
      name: "Range inner saturation edge",
    });
    fireEvent.keyDown(edge, { key: "ArrowUp", shiftKey: true });
    expect(lastRanges()[0].chromaRange).toBe(60);
    const colour = screen.getByRole("slider", { name: "Range color" });
    fireEvent.keyDown(colour, { key: "ArrowLeft" });
    expect(lastRanges()[0].hue).toBe(49);
  });

  it("a press beside the handles moves the range to that colour", () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    const svg = wheelSvg();
    fireEvent.pointerDown(svg, { button: 0, ...at(200, 0.1) });
    fireEvent.pointerUp(svg);
    const moved = lastRanges()[0];
    expect(moved.hue).toBeCloseTo(200, 0);
    expect(moved.chroma).toBeCloseTo(0.1, 2);
    // Lightness and shape stay.
    expect(moved.lightness).toBe(0.65);
    expect(moved.hueRange).toBe(25);
  });

  it("a pick armed for a range ends with a press on the wheel", () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    fireEvent.click(
      screen.getByRole("button", { name: "Pick the range color again" }),
    );
    const svg = wheelSvg();
    fireEvent.pointerDown(svg, { button: 0, ...at(120, 0.1) });
    fireEvent.pointerUp(svg);
    expect(lastRanges()).toHaveLength(1);
    expect(lastRanges()[0].hue).toBeCloseTo(120, 0);
    expect(lastRanges()[0].lightness).toBe(0.65);
    expect(useEditorStore.getState().colorPickerTarget).toBeNull();
  });

  it("re-picks a range on the wheel at the range limit", () => {
    const full = Array.from({ length: 8 }, (_, i) => ({
      ...newColorRange(i * 45, 0.08, 0.6),
      hueRange: 10,
    }));
    render(<Harness initial={full} />);
    openTool();
    fireEvent.click(
      screen.getByRole("button", { name: "Pick the range color again" }),
    );
    const group = screen.getByRole("group", { name: "Color range wheel" });
    expect(group.className).not.toContain("is-disabled");
    const svg = wheelSvg();
    fireEvent.pointerDown(svg, { button: 0, ...at(20, 0.1) });
    expect(lastRanges()).toHaveLength(8);
    expect(lastRanges()[0].hue).toBeCloseTo(20, 0);
    expect(useEditorStore.getState().colorPickerTarget).toBeNull();
    // Adding a ninth stays impossible.
    expect(
      (
        screen.getByRole("button", {
          name: "Pick a color range",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
  });

  it("with the picker armed, a press on the wheel adds a range", () => {
    render(<Harness initial={[skinish()]} />);
    openTool();
    fireEvent.click(
      screen.getByRole("button", { name: "Pick a color range" }),
    );
    expect(useEditorStore.getState().colorPickerTarget).toBe("new");
    const svg = wheelSvg();
    fireEvent.pointerDown(svg, { button: 0, ...at(140, 0.12) });
    expect(lastRanges()).toHaveLength(2);
    expect(lastRanges()[1].hue).toBeCloseTo(140, 0);
    expect(useEditorStore.getState().colorPickerTarget).toBeNull();
    expect(useEditorStore.getState().activeColorRangeId).toBe(
      lastRanges()[1].id,
    );
  });
});

describe("skin tone mode", () => {
  it("picks skin on the wheel into a range of its own", () => {
    const other = newColorRange(240, 0.1, 0.5);
    render(<Harness initial={[other]} />);
    openTool();
    fireEvent.click(screen.getByRole("tab", { name: "Skin tone" }));
    expect(screen.getByText(/Pick skin in the photo/)).toBeTruthy();
    // No skin range yet: the wheel shows none and a press picks skin.
    expect(screen.queryByRole("slider", { name: "Range color" })).toBeNull();
    const svg = wheelSvg();
    act(() => {
      fireEvent.pointerDown(svg, { button: 0, ...at(55, 0.07) });
    });
    const ranges = lastRanges();
    expect(ranges).toHaveLength(2);
    const skin = ranges.find((r) => r.kind === "skin")!;
    expect(skin.hue).toBeCloseTo(55, 0);
    expect(useEditorStore.getState().activeColorRangeId).toBe(skin.id);
    // Uniformity comes first, the shifts under Amount after it.
    const names = screen
      .getAllByRole("slider")
      .map((s) => s.getAttribute("aria-label"));
    expect(names.indexOf("Color range hue uniformity")).toBeLessThan(
      names.indexOf("Color range hue"),
    );
    expect(screen.getByText("Amount")).toBeTruthy();
    fireEvent.keyDown(
      screen.getByRole("slider", { name: "Color range saturation uniformity" }),
      { key: "ArrowRight" },
    );
    expect(
      lastRanges().find((r) => r.kind === "skin")!.saturationUniformity,
    ).toBe(1);
    expect(lastRanges().find((r) => r.id === other.id)).toEqual(other);
  });

  it("arms the photo picker for skin and keeps skin out of the range list", () => {
    const skin = { ...newColorRange(50, 0.06, 0.6), kind: "skin" as const };
    const other = newColorRange(240, 0.1, 0.5);
    render(<Harness initial={[skin, other]} />);
    openTool();
    // Ranges mode lists the other range only.
    expect(
      screen.getAllByRole("button", { name: /^Color range \d+°$/ }),
    ).toHaveLength(1);
    fireEvent.click(screen.getByRole("tab", { name: "Skin tone" }));
    expect(useEditorStore.getState().activeColorRangeId).toBe(skin.id);
    fireEvent.click(
      screen.getByRole("button", { name: "Pick the range color again" }),
    );
    expect(useEditorStore.getState().colorPickerTarget).toBe(skin.id);
    fireEvent.click(screen.getByRole("tab", { name: "Ranges" }));
    expect(useEditorStore.getState().colorPickerTarget).toBeNull();
  });

  it("offers the skin picker when there is no skin range", () => {
    render(<Harness initial={[]} />);
    openTool();
    fireEvent.click(screen.getByRole("tab", { name: "Skin tone" }));
    fireEvent.click(screen.getByRole("button", { name: "Pick the skin tone" }));
    expect(useEditorStore.getState().colorPickerTarget).toBe("skin");
    expect(screen.getByText("Click skin in the photo.")).toBeTruthy();
  });
});
