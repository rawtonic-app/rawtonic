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
