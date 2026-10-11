import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  act,
} from "@testing-library/react";
import AdjustmentControl from "./AdjustmentControl";
import { histogramBins } from "./StudioHistogram";
import { visibleRange } from "./StudioBrowser";

beforeEach(() =>
  vi.stubGlobal("requestAnimationFrame", (fn: FrameRequestCallback) =>
    setTimeout(() => fn(0), 16),
  ),
);
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
describe("professional adjustment control", () => {
  it("clamps typed input, rejects invalid input and cancels on Escape", () => {
    const change = vi.fn();
    render(
      <AdjustmentControl
        label="Exposure"
        value={1}
        min={-5}
        max={5}
        step={0.01}
        onChange={change}
      />,
    );
    const input = screen.getByRole("textbox");
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "12" } });
    fireEvent.blur(input);
    expect(change).toHaveBeenLastCalledWith(5);
    change.mockClear();
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "invalid" } });
    fireEvent.blur(input);
    expect(change).toHaveBeenLastCalledWith(1);
    change.mockClear();
    input.focus();
    fireEvent.change(input, { target: { value: "-4" } });
    fireEvent.keyDown(input, { key: "Escape" });
    expect(change).not.toHaveBeenCalled();
    expect((input as HTMLInputElement).value).toBe("1");
  });
  it("supports fine and coarse keyboard steps and reset", () => {
    const change = vi.fn();
    render(<AdjustmentControl label="Contrast" value={0} onChange={change} />);
    const slider = screen.getByRole("slider");
    fireEvent.keyDown(slider, { key: "ArrowRight", altKey: true });
    expect(change).toHaveBeenLastCalledWith(0.1);
    fireEvent.keyDown(slider, { key: "ArrowRight", shiftKey: true });
    expect(change).toHaveBeenLastCalledWith(10);
    fireEvent.doubleClick(slider);
    expect(change).toHaveBeenLastCalledWith(0);
  });
  it("coalesces drag updates and flushes the final value on release", () => {
    vi.useFakeTimers();
    const change = vi.fn();
    const interaction = vi.fn();
    render(
      <AdjustmentControl
        label="Contrast"
        value={0}
        onChange={change}
        onInteraction={interaction}
      />,
    );
    const slider = screen.getByRole("slider");
    fireEvent.pointerDown(slider);
    [1, 2, 3, 4].forEach((value) =>
      fireEvent.change(slider, { target: { value } }),
    );
    expect(change).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(16));
    expect(change).toHaveBeenCalledTimes(1);
    expect(change).toHaveBeenLastCalledWith(4);
    fireEvent.change(slider, { target: { value: 7 } });
    fireEvent.pointerUp(slider);
    expect(change).toHaveBeenLastCalledWith(7);
    expect(interaction).toHaveBeenLastCalledWith(false);
  });
});
describe("virtual photo browser", () => {
  it("bounds visible work for a 100,000 photograph library", () => {
    const range = visibleRange(100000, 50000, 800, 200, 5);
    expect(range.end - range.start).toBe(30);
    expect(range.start).toBe(1245);
  });
  it("handles empty libraries and the final partial row", () => {
    expect(visibleRange(0, 0, 800, 200, 5)).toEqual({ start: 0, end: 0 });
    expect(visibleRange(13, 400, 800, 200, 5).end).toBe(13);
  });
});

describe("native histogram adapter", () => {
  it("reads native channel arrays and the wrapped curve form", () => {
    expect(histogramBins([0, 10, 20])).toEqual([0, 10, 20]);
    expect(histogramBins({ data: [0, 10, 20] })).toEqual([0, 10, 20]);
    expect(histogramBins(null)).toEqual([]);
    expect(histogramBins([NaN, -4, 12])).toEqual([0, 0, 12]);
  });
});
