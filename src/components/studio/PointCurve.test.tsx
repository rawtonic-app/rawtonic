import "../../i18n";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";
import { useEditorStore } from "../../store/useEditorStore";
import { PointCurve } from "./StudioDevelop";

afterEach(() => {
  cleanup();
  useEditorStore.setState({
    curveChannel: "luma",
    tatTarget: null,
    tatCurveMarker: null,
  });
});

describe("point curve targeted marker", () => {
  it("marks the sampled input on the shown channel's curve, only for the targeted curve", () => {
    const curves = {
      ...INITIAL_ADJUSTMENTS.curves,
      red: [
        { x: 0, y: 0 },
        { x: 100, y: 140 },
        { x: 255, y: 255 },
      ],
    };
    useEditorStore.setState({
      curveChannel: "red",
      tatTarget: "curve",
      tatCurveMarker: 100,
    });
    const { container, rerender } = render(
      <PointCurve curves={curves} onChange={() => {}} disabled={false} onInteraction={() => {}} targeted />,
    );
    // The channel tab is the store's (the targeted curve moves that one).
    expect(screen.getByRole("button", { name: "R" }).getAttribute("aria-pressed")).toBe("true");
    const marker = screen.getByTestId("curve-target-marker");
    expect(marker.querySelector("line")!.getAttribute("x1")).toBe("100");
    expect(marker.querySelector("circle")!.getAttribute("cy")).toBe(String(255 - 140));
    // Another channel: the old channel's input is not shown on it.
    fireEvent.click(screen.getByRole("button", { name: "G" }));
    expect(useEditorStore.getState().curveChannel).toBe("green");
    expect(container.querySelector(".curve-target")).toBeNull();
    act(() => useEditorStore.setState({ tatCurveMarker: 100 }));
    expect(screen.getByTestId("curve-target-marker").querySelector("circle")!.getAttribute("cy")).toBe("155");
    // A layer's curves (not the targeted ones) and other targets: no marker.
    rerender(<PointCurve curves={curves} onChange={() => {}} disabled={false} onInteraction={() => {}} />);
    expect(container.querySelector(".curve-target")).toBeNull();
    rerender(<PointCurve curves={curves} onChange={() => {}} disabled={false} onInteraction={() => {}} targeted />);
    act(() => useEditorStore.setState({ tatTarget: "regionCurve" }));
    expect(container.querySelector(".curve-target")).toBeNull();
  });
});

describe("point curve keyboard", () => {
  it("uses the drawing transform for creation and dragging in every channel", () => {
    vi.stubGlobal("PointerEvent", MouseEvent);
    const changed = vi.fn();
    function Harness() {
      const [curves, setCurves] = useState(INITIAL_ADJUSTMENTS.curves);
      return <PointCurve curves={curves} onChange={(next) => { changed(next); setCurves(next); }} disabled={false} onInteraction={() => {}} />;
    }
    const { container } = render(<Harness />);
    const svg = container.querySelector("svg")!;
    Object.assign(svg, {
      getScreenCTM: () => ({ inverse: () => ({ a: 255 / 210, b: 0, c: 0, d: 255 / 210, e: -32.5 * 255 / 210, f: 0 }) }),
      setPointerCapture: () => {},
      hasPointerCapture: () => false,
    });
    for (const [button, channel] of [["RGB", "luma"], ["Luminance", "luminance"], ["R", "red"], ["G", "green"], ["B", "blue"]] as const) {
      fireEvent.click(screen.getByRole("button", { name: button }));
      fireEvent.pointerDown(svg, { button: 0, clientX: 75, clientY: 105 });
      expect(changed.mock.lastCall![0][channel][1].x).toBeCloseTo(42.5 * 255 / 210);
      fireEvent.pointerMove(svg, { buttons: 1, clientX: 100, clientY: 100 });
      fireEvent.pointerUp(svg);
      expect(changed.mock.lastCall![0][channel][1].x).toBeCloseTo(67.5 * 255 / 210);
    }
    vi.unstubAllGlobals();
  });
  it("adds, moves and removes a point and keeps endpoint constraints", () => {
    const changed = vi.fn();
    function Harness() {
      const [curves, setCurves] = useState(INITIAL_ADJUSTMENTS.curves);
      return <PointCurve curves={curves} onChange={(next) => { changed(next); setCurves(next); }} disabled={false} onInteraction={() => {}} />;
    }
    render(<Harness />);
    const first = screen.getByRole("button", { name: /Point 1, input 0, output 0/i });
    fireEvent.focus(first);
    fireEvent.keyDown(first, { key: "ArrowLeft" });
    expect(changed).not.toHaveBeenCalled();
    fireEvent.keyDown(first, { key: "a" });
    expect(changed).toHaveBeenCalledTimes(1);
    const added = screen.getByRole("button", { name: /Point 2, input 128, output 128/i });
    fireEvent.focus(added);
    fireEvent.keyDown(added, { key: "ArrowUp" });
    expect(changed).toHaveBeenCalledTimes(2);
    const moved = screen.getByRole("button", { name: /Point 2, input 128, output 129/i });
    fireEvent.keyDown(moved, { key: "Delete" });
    expect(changed).toHaveBeenCalledTimes(3);
    expect(screen.queryByRole("button", { name: /Point 2, input 128/i })).toBeNull();
    fireEvent.keyDown(first, { key: "Delete" });
    expect(changed).toHaveBeenCalledTimes(3);
  });
});

describe("point curve reading aids (UXR1 D3)", () => {
  const histogram = {
    luma: [0, 4, 9, 4, 0],
    red: [9, 0, 0, 0, 9],
    green: [0, 0, 9, 0, 0],
    blue: [0, 9, 0, 9, 0],
  };
  afterEach(() => useEditorStore.setState({ histogram: null }));

  it("draws the shown channel's histogram behind the curve", () => {
    useEditorStore.setState({ histogram: histogram as never });
    const { container } = render(
      <PointCurve curves={INITIAL_ADJUSTMENTS.curves} onChange={() => {}} disabled={false} onInteraction={() => {}} />,
    );
    const shown = () => container.querySelector(".curve-histogram");
    expect(shown()?.getAttribute("data-channel")).toBe("luma");
    // Behind everything else: the first thing drawn.
    expect(container.querySelector("svg")!.firstElementChild).toBe(shown());
    fireEvent.click(screen.getByRole("button", { name: "R" }));
    expect(shown()?.getAttribute("data-channel")).toBe("red");
    act(() => useEditorStore.setState({ histogram: null }));
    expect(shown()).toBeNull();
  });

  it("shows a crosshair through the dragged point only while it is dragged", () => {
    vi.stubGlobal("PointerEvent", MouseEvent);
    function Harness() {
      const [curves, setCurves] = useState(INITIAL_ADJUSTMENTS.curves);
      return <PointCurve curves={curves} onChange={setCurves} disabled={false} onInteraction={() => {}} />;
    }
    const { container } = render(<Harness />);
    const svg = container.querySelector("svg")!;
    Object.assign(svg, {
      getScreenCTM: () => ({ inverse: () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 }) }),
      setPointerCapture: () => {},
      hasPointerCapture: () => false,
    });
    fireEvent.pointerDown(svg, { button: 0, clientX: 64, clientY: 191 });
    fireEvent.pointerMove(svg, { buttons: 1, clientX: 64, clientY: 211 });
    const cross = container.querySelector(".curve-crosshair")!;
    expect(cross).not.toBeNull();
    const [vertical, horizontal] = [...cross.querySelectorAll("line")];
    expect(vertical.getAttribute("x1")).toBe("64");
    expect(horizontal.getAttribute("y1")).toBe("211");
    fireEvent.pointerUp(svg);
    expect(container.querySelector(".curve-crosshair")).toBeNull();
    vi.unstubAllGlobals();
  });
});

describe("region curve split handles under the curve (UXR1 D4)", () => {
  const region = {
    highlights: 0,
    lights: 0,
    darks: 0,
    shadows: 0,
    shadowSplit: 25,
    midtoneSplit: 50,
    highlightSplit: 75,
  };
  const mount = () => {
    const onRegionChange = vi.fn();
    function Harness() {
      const [value, setValue] = useState(region);
      return (
        <PointCurve
          curves={INITIAL_ADJUSTMENTS.curves}
          region={value}
          onRegionChange={(next) => {
            onRegionChange(next);
            setValue(next);
          }}
          onChange={() => {}}
          disabled={false}
          onInteraction={() => {}}
        />
      );
    }
    const view = render(<Harness />);
    return { ...view, onRegionChange };
  };
  const handle = (name: string) => screen.getByRole("slider", { name });

  it("shows three handles with their values on the RGB curve only", () => {
    mount();
    expect(handle("Region curve shadow split").getAttribute("aria-valuenow")).toBe("25");
    expect(handle("Region curve midtone split").getAttribute("aria-valuenow")).toBe("50");
    expect(handle("Region curve highlight split").getAttribute("aria-valuenow")).toBe("75");
    // The number is on the handle (shown on hover/focus by the stylesheet).
    expect(handle("Region curve midtone split").textContent).toContain("50");
    fireEvent.click(screen.getByRole("button", { name: "R" }));
    expect(screen.queryByRole("slider", { name: "Region curve shadow split" })).toBeNull();
  });

  it("moves a handle with the arrow keys within its range", () => {
    const { onRegionChange } = mount();
    fireEvent.keyDown(handle("Region curve shadow split"), { key: "ArrowRight" });
    expect(onRegionChange).toHaveBeenLastCalledWith({ ...region, shadowSplit: 26 });
    fireEvent.keyDown(handle("Region curve shadow split"), { key: "ArrowRight", shiftKey: true });
    fireEvent.keyDown(handle("Region curve shadow split"), { key: "ArrowRight", shiftKey: true });
    expect(onRegionChange).toHaveBeenLastCalledWith({ ...region, shadowSplit: 45 });
  });

  it("drags a handle along the curve's input axis", () => {
    vi.stubGlobal("PointerEvent", MouseEvent);
    const { container, onRegionChange } = mount();
    const svg = container.querySelector("svg")!;
    Object.assign(svg, {
      getScreenCTM: () => ({ inverse: () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 }) }),
      setPointerCapture: () => {},
      hasPointerCapture: () => false,
    });
    const midtone = handle("Region curve midtone split");
    Object.assign(midtone, { setPointerCapture: () => {}, hasPointerCapture: () => false });
    fireEvent.pointerDown(midtone, { button: 0, clientX: 127.5, clientY: 265 });
    // 40 % of the input axis.
    fireEvent.pointerMove(midtone, { buttons: 1, clientX: 102, clientY: 265 });
    fireEvent.pointerUp(midtone);
    expect(onRegionChange).toHaveBeenLastCalledWith({ ...region, midtoneSplit: 40 });
    // The press did not add a curve point.
    expect(screen.queryByRole("button", { name: /Point 3/ })).toBeNull();
    vi.unstubAllGlobals();
  });

  describe("a drag against a committed value that lags", () => {
    const setup = () => {
      vi.stubGlobal("PointerEvent", MouseEvent);
      const onInteraction = vi.fn();
      // The parent never updates `region`: the 250 ms draft has not committed.
      const view = render(
        <PointCurve
          curves={INITIAL_ADJUSTMENTS.curves}
          region={region}
          onRegionChange={() => {}}
          onChange={() => {}}
          disabled={false}
          onInteraction={onInteraction}
        />,
      );
      Object.assign(view.container.querySelector("svg")!, {
        getScreenCTM: () => ({ inverse: () => ({ a: 1, b: 0, c: 0, d: 1, e: 0, f: 0 }) }),
      });
      const midtone = handle("Region curve midtone split");
      Object.assign(midtone, { setPointerCapture: () => {}, hasPointerCapture: () => false });
      fireEvent.pointerDown(midtone, { button: 0, clientX: 127.5, clientY: 265 });
      fireEvent.pointerMove(midtone, { buttons: 1, clientX: 102, clientY: 265 });
      return { ...view, midtone, onInteraction };
    };
    afterEach(() => vi.unstubAllGlobals());

    it("draws the handle at the pointer, not at the committed value", () => {
      const { midtone } = setup();
      expect(midtone.getAttribute("aria-valuenow")).toBe("40");
      expect(midtone.textContent).toContain("40");
    });

    it("gives the interaction back on pointercancel", () => {
      const { midtone, onInteraction } = setup();
      fireEvent.pointerCancel(midtone);
      expect(onInteraction).toHaveBeenLastCalledWith(false);
      expect(onInteraction).toHaveBeenCalledTimes(2);
      expect(midtone.getAttribute("aria-valuenow")).toBe("50");
    });

    it("gives the interaction back when the curve unmounts mid-drag", () => {
      const { unmount, onInteraction } = setup();
      unmount();
      expect(onInteraction).toHaveBeenLastCalledWith(false);
      expect(onInteraction).toHaveBeenCalledTimes(2);
    });
  });

  it("takes an exact value after a double-click", () => {
    const { onRegionChange } = mount();
    fireEvent.doubleClick(handle("Region curve highlight split"));
    const field = screen.getByRole("spinbutton", { name: "Region curve highlight split" });
    fireEvent.change(field, { target: { value: "80" } });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(onRegionChange).toHaveBeenLastCalledWith({ ...region, highlightSplit: 80 });
    expect(screen.queryByRole("spinbutton", { name: "Region curve highlight split" })).toBeNull();
    // Escape leaves the value as it was.
    fireEvent.doubleClick(handle("Region curve highlight split"));
    const again = screen.getByRole("spinbutton", { name: "Region curve highlight split" });
    fireEvent.change(again, { target: { value: "60" } });
    fireEvent.keyDown(again, { key: "Escape" });
    expect(onRegionChange).toHaveBeenCalledTimes(1);
  });
});
