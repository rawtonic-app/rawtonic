import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  act,
} from "@testing-library/react";
import AdjustmentControl, { dragPrecision } from "./AdjustmentControl";
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
  // A track 109 px wide: 100 px of thumb travel (9 px thumb), at 0 the
  // thumb's centre sits at 4.5 + 50 = 54.5.
  const box = { left: 0, top: 0, width: 109, height: 20 } as DOMRect;
  const exposure = (change = vi.fn()) => {
    render(
      <AdjustmentControl label="Exposure" value={0} min={-5} max={5} step={0.01} onChange={change} />,
    );
    const slider = screen.getByRole("slider", { name: "Exposure" }) as HTMLInputElement;
    vi.spyOn(slider, "getBoundingClientRect").mockReturnValue(box);
    slider.setPointerCapture = vi.fn();
    return { slider, change };
  };
  it("grabs the thumb without a jump and moves the track's own distance along it", () => {
    const { slider, change } = exposure();
    // A press 3 px off the thumb's centre: no change (the native slider jumped).
    const press = fireEvent.pointerDown(slider, { button: 0, clientX: 57.5, clientY: 10, pointerId: 1 });
    expect(press).toBe(false);
    expect(change).not.toHaveBeenCalled();
    // 20 px along the track = 20 % of the range.
    fireEvent.pointerMove(slider, { clientX: 77.5, clientY: 10, pointerId: 1 });
    fireEvent.pointerUp(slider, { pointerId: 1 });
    expect(change).toHaveBeenLastCalledWith(2);
    // Codex r1: the suppressed native press must still move keyboard focus.
    expect(document.activeElement).toBe(slider);
    expect(slider.closest(".adjustment-control")!.classList.contains("is-active")).toBe(false);
  });
  it("moves the thumb to a press beside it, then drags from there", () => {
    const calls: string[] = [];
    const change = vi.fn((v: number) => calls.push(`change ${v}`));
    render(
      <AdjustmentControl
        label="Exposure"
        value={0}
        min={-5}
        max={5}
        step={0.01}
        onChange={change}
        onInteraction={(active) => calls.push(`interaction ${active}`)}
      />,
    );
    const slider = screen.getByRole("slider", { name: "Exposure" }) as HTMLInputElement;
    vi.spyOn(slider, "getBoundingClientRect").mockReturnValue(box);
    slider.setPointerCapture = vi.fn();
    fireEvent.pointerDown(slider, { button: 0, clientX: 14.5, clientY: 10, pointerId: 1 });
    expect(change).toHaveBeenLastCalledWith(-4);
    // Codex r1: the jump belongs to the drag (one history step), so the
    // interaction starts before its value is sent.
    expect(calls.slice(0, 2)).toEqual(["interaction true", "change -4"]);
    fireEvent.pointerMove(slider, { clientX: 24.5, clientY: 10, pointerId: 1 });
    fireEvent.pointerUp(slider, { pointerId: 1 });
    expect(change).toHaveBeenLastCalledWith(-3);
  });
  it("moves less per pixel the farther the pointer is from the track", () => {
    const { slider, change } = exposure();
    fireEvent.pointerDown(slider, { button: 0, clientX: 54.5, clientY: 10, pointerId: 1 });
    // 96 px below the track's centre: a quarter, 20 px = 0.5 EV.
    fireEvent.pointerMove(slider, { clientX: 54.5, clientY: 106, pointerId: 1 });
    fireEvent.pointerMove(slider, { clientX: 74.5, clientY: 106, pointerId: 1 });
    expect(change).toHaveBeenLastCalledWith(0.5);
    // Back on the track the next 10 px are at full speed; nothing jumps.
    fireEvent.pointerMove(slider, { clientX: 74.5, clientY: 10, pointerId: 1 });
    expect(change).toHaveBeenLastCalledWith(0.5);
    fireEvent.pointerMove(slider, { clientX: 84.5, clientY: 10, pointerId: 1 });
    fireEvent.pointerUp(slider, { pointerId: 1 });
    expect(change).toHaveBeenLastCalledWith(1.5);
    expect(dragPrecision(0)).toBe(1);
    expect(dragPrecision(24)).toBe(1);
    expect(dragPrecision(48)).toBe(0.5);
    expect(dragPrecision(1000)).toBe(0.1);
  });
  it("moves a tenth as far per pixel when the track is dragged with Option", () => {
    const { slider, change } = exposure();
    // With Option even a press beside the thumb does not jump.
    const fine = fireEvent.pointerDown(slider, { button: 0, clientX: 20, clientY: 10, altKey: true, pointerId: 2 });
    expect(fine).toBe(false);
    fireEvent.pointerMove(slider, { clientX: 40, clientY: 10, altKey: true, pointerId: 2 });
    fireEvent.pointerUp(slider, { pointerId: 2 });
    expect(change).toHaveBeenLastCalledWith(0.2);
  });
  it("starts an Option-drag from the value just typed, not the old one", () => {
    const change = vi.fn();
    render(
      <AdjustmentControl label="Exposure" value={0} min={-5} max={5} step={0.01} onChange={change} />,
    );
    const slider = screen.getByRole("slider", { name: "Exposure" }) as HTMLInputElement;
    vi.spyOn(slider, "getBoundingClientRect").mockReturnValue({ left: 0, top: 0, width: 109, height: 20 } as DOMRect);
    slider.setPointerCapture = vi.fn();
    const field = screen.getByRole("textbox");
    field.focus();
    fireEvent.change(field, { target: { value: "2" } });
    // The press moves focus off the field, which commits the typed 2.
    fireEvent.pointerDown(slider, { button: 0, clientX: 50, altKey: true, pointerId: 3 });
    fireEvent.pointerMove(slider, { clientX: 70, altKey: true, pointerId: 3 });
    fireEvent.pointerUp(slider, { pointerId: 3 });
    expect(change).toHaveBeenLastCalledWith(2.2);
  });
  it("marks its row while the slider or its label is dragged, and only then", () => {
    // The pressed state must read without the tooltip (state-legibility rule):
    // the stylesheet lights the label and enlarges the thumb on `is-active`.
    render(
      <AdjustmentControl label="Exposure" value={0} min={-5} max={5} step={0.01} onChange={vi.fn()} />,
    );
    const slider = screen.getByRole("slider", { name: "Exposure" });
    const row = slider.closest(".adjustment-control")!;
    expect(row.classList.contains("is-active")).toBe(false);
    fireEvent.pointerDown(slider, { button: 0 });
    expect(row.classList.contains("is-active")).toBe(true);
    fireEvent.pointerUp(slider);
    expect(row.classList.contains("is-active")).toBe(false);
    const label = screen.getByText("Exposure");
    // jsdom has no pointer capture.
    label.setPointerCapture = vi.fn();
    fireEvent.pointerDown(label, { button: 0, clientX: 10, pointerId: 1 });
    expect(row.classList.contains("is-active")).toBe(true);
    fireEvent.pointerUp(label, { pointerId: 1 });
    expect(row.classList.contains("is-active")).toBe(false);
    // Typing a value is no drag.
    const field = screen.getByRole("textbox");
    fireEvent.focus(field);
    expect(row.classList.contains("is-active")).toBe(false);
  });
  it("leaves a stored value outside its range alone when the field is only focused", () => {
    // Codex r1: Brightness was ±5; a stored 3 must survive a focus and blur.
    const change = vi.fn();
    render(<AdjustmentControl label="Brightness" value={3} min={-2} max={2} step={0.01} onChange={change} />);
    const field = screen.getByRole("textbox");
    expect((field as HTMLInputElement).value).toBe("3");
    fireEvent.focus(field);
    fireEvent.blur(field);
    expect(change).not.toHaveBeenCalled();
    // Typing a value is clamped to the range as before.
    fireEvent.focus(field);
    fireEvent.change(field, { target: { value: "3.5" } });
    fireEvent.blur(field);
    expect(change).toHaveBeenLastCalledWith(2);
  });
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
  it("keeps the value selected after the click that focuses it", () => {
    render(<AdjustmentControl label="Exposure" value={1.25} onChange={() => {}} />);
    const input = screen.getByRole("textbox") as HTMLInputElement;
    // The focusing click: its mouse-up must not collapse the selection
    // (WebKit does unless the default is prevented), so typing replaces the
    // value without Cmd+A (notes: reference/wp7-use-test-c1-lr.md finding 1).
    fireEvent.mouseDown(input);
    input.focus();
    expect([input.selectionStart, input.selectionEnd]).toEqual([0, input.value.length]);
    expect(fireEvent.mouseUp(input)).toBe(false);
    // A second click into the focused field places the caret as usual.
    fireEvent.mouseDown(input);
    expect(fireEvent.mouseUp(input)).toBe(true);
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
  it("sends drag input at once, coalesces faster input and flushes the final value on release", () => {
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
    // The first move goes out with its event; moves within 8 ms of it
    // wait for the next animation frame and only the newest is sent.
    [1, 2, 3, 4].forEach((value) =>
      fireEvent.change(slider, { target: { value } }),
    );
    expect(change).toHaveBeenCalledTimes(1);
    expect(change).toHaveBeenLastCalledWith(1);
    act(() => vi.advanceTimersByTime(16));
    expect(change).toHaveBeenCalledTimes(2);
    expect(change).toHaveBeenLastCalledWith(4);
    // A move a 60 Hz mouse later is sent with its event.
    act(() => vi.advanceTimersByTime(16));
    fireEvent.change(slider, { target: { value: 5 } });
    expect(change).toHaveBeenCalledTimes(3);
    expect(change).toHaveBeenLastCalledWith(5);
    act(() => vi.advanceTimersByTime(1));
    fireEvent.change(slider, { target: { value: 7 } });
    expect(change).toHaveBeenCalledTimes(3);
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
