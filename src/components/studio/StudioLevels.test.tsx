import "../../i18n";
import { useState } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import {
  INITIAL_ADJUSTMENTS,
  INITIAL_LEVELS,
  type Adjustments,
} from "../../utils/adjustments";
import { useEditorStore } from "../../store/useEditorStore";
import { linearFromSrgb } from "../../utils/targetedAdjustment";
import TargetedAdjustmentOverlay from "../panel/editor/TargetedAdjustmentOverlay";
import { LEVELS_HISTOGRAM_DELAY_MS, StudioLevels } from "./StudioLevels";

beforeAll(() => {
  // jsdom has no pointer capture.
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
});
beforeEach(() => {
  useEditorStore.setState({
    selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    tatTarget: null,
    levelsChannel: "rgb",
    histogram: null,
  });
});
afterEach(() => {
  cleanup();
  invoke.mockReset();
});

/** The tool with its values held like the panel holds them; `deferred`
 * holds commits back like sliderDraft does during a drag. */
function renderLevels(deferred = false) {
  const changes: Partial<Adjustments>[] = [];
  const interactions: boolean[] = [];
  function Harness() {
    const [values, setValues] = useState<Partial<Adjustments>>({
      levels: structuredClone(INITIAL_LEVELS),
    });
    return (
      <StudioLevels
        values={values}
        disabled={false}
        change={(patch) => {
          changes.push(patch);
          if (!deferred) setValues((v) => ({ ...v, ...patch }));
        }}
        onInteraction={(active) => interactions.push(active)}
      />
    );
  }
  render(<Harness />);
  const last = () => changes.at(-1)?.levels;
  return { changes, interactions, last };
}

const handle = (name: string) => screen.getByRole("slider", { name });
const field = (name: string) =>
  screen.getByRole("textbox", { name: `${name} value` }) as HTMLInputElement;
/** Handles measure their strip: 255 px wide from x = 0. */
const strip = (el: HTMLElement) => {
  el.parentElement!.getBoundingClientRect = () =>
    ({ left: 0, top: 0, width: 255, height: 12 }) as DOMRect;
};

describe("Levels panel", () => {
  it("names every handle with its value", () => {
    renderLevels();
    for (const [name, now] of [
      ["Input black", 0],
      ["Midtone", 0],
      ["Input white", 255],
      ["Output black", 0],
      ["Output white", 255],
    ] as const) {
      expect(handle(name).getAttribute("aria-valuenow")).toBe(String(now));
      expect(field(name).value).toBe(String(now));
    }
  });

  it("drags a handle as one undo step and pushes the other point", () => {
    const { changes, interactions, last } = renderLevels();
    const black = handle("Input black");
    strip(black);
    fireEvent.pointerDown(black, { button: 0, clientX: 0, pointerId: 1 });
    fireEvent.pointerMove(black, { clientX: 40, pointerId: 1 });
    fireEvent.pointerMove(black, { clientX: 60, pointerId: 1 });
    fireEvent.pointerUp(black, { pointerId: 1 });
    expect(interactions).toEqual([true, false]);
    expect(changes).toHaveLength(2);
    expect(last()?.rgb).toMatchObject({ inBlack: 60, inWhite: 255 });
    expect(handle("Input black").getAttribute("aria-valuenow")).toBe("60");
    expect(field("Input black").value).toBe("60");
  });

  it("follows a drag before its moves are committed", () => {
    const { last } = renderLevels(true);
    const black = handle("Input black");
    strip(black);
    fireEvent.pointerDown(black, { button: 0, clientX: 0, pointerId: 1 });
    fireEvent.pointerMove(black, { clientX: 40, pointerId: 1 });
    expect(last()?.rgb.inBlack).toBe(40);
    expect(handle("Input black").getAttribute("aria-valuenow")).toBe("40");
    expect(handle("Input black").style.left).toBe(`${(40 / 255) * 100}%`);
    expect(field("Input black").value).toBe("40");
    fireEvent.pointerUp(black, { pointerId: 1 });
  });

  it("nudges with the arrow keys, Shift by ten", () => {
    const { last } = renderLevels();
    fireEvent.keyDown(handle("Input white"), { key: "ArrowLeft" });
    expect(last()?.rgb.inWhite).toBe(254);
    fireEvent.keyDown(handle("Input white"), { key: "ArrowLeft", shiftKey: true });
    expect(last()?.rgb.inWhite).toBe(244);
    fireEvent.keyDown(field("Midtone"), { key: "ArrowUp", shiftKey: true });
    expect(last()?.rgb.gamma).toBe(1.149);
  });

  it("takes typed values with the sliders' clamping, Escape cancels", () => {
    const { changes, last } = renderLevels();
    const white = field("Input white");
    fireEvent.focus(white);
    fireEvent.change(white, { target: { value: "180" } });
    fireEvent.keyDown(white, { key: "Enter" });
    fireEvent.blur(white);
    expect(last()?.rgb.inWhite).toBe(180);
    const black = field("Input black");
    fireEvent.focus(black);
    fireEvent.change(black, { target: { value: "300" } });
    fireEvent.blur(black);
    expect(last()?.rgb).toMatchObject({ inBlack: 254, inWhite: 255 });
    const count = changes.length;
    fireEvent.focus(black);
    fireEvent.change(black, { target: { value: "12" } });
    fireEvent.keyDown(black, { key: "Escape" });
    fireEvent.blur(black);
    expect(changes).toHaveLength(count);
    expect(field("Input black").value).toBe("254");
  });

  it("resets a value on double-click of its handle or field", () => {
    const { last } = renderLevels();
    fireEvent.keyDown(handle("Output black"), { key: "ArrowRight", shiftKey: true });
    expect(last()?.rgb.outBlack).toBe(10);
    fireEvent.doubleClick(handle("Output black"));
    expect(last()?.rgb.outBlack).toBe(0);
    fireEvent.keyDown(handle("Midtone"), { key: "ArrowDown" });
    fireEvent.doubleClick(field("Midtone"));
    expect(last()?.rgb.gamma).toBe(1);
  });

  it("resets a focused field without its old text coming back", () => {
    const { changes, last } = renderLevels();
    const black = field("Input black");
    fireEvent.focus(black);
    fireEvent.change(black, { target: { value: "10" } });
    fireEvent.keyDown(black, { key: "Enter" });
    fireEvent.blur(black);
    expect(last()?.rgb.inBlack).toBe(10);
    fireEvent.focus(black);
    fireEvent.doubleClick(black);
    expect(field("Input black").value).toBe("0");
    fireEvent.blur(black);
    expect(changes.map((c) => c.levels?.rgb.inBlack)).toEqual([10, 0]);
    expect(field("Input black").value).toBe("0");
  });

  it("edits the channel of the active tab", () => {
    const { last } = renderLevels();
    fireEvent.click(screen.getByRole("button", { name: "G" }));
    expect(useEditorStore.getState().levelsChannel).toBe("green");
    fireEvent.keyDown(handle("Input black"), { key: "ArrowRight" });
    expect(last()?.green.inBlack).toBe(1);
    expect(last()?.rgb).toEqual(INITIAL_LEVELS.rgb);
  });

  it("turns the pickers on and off, one at a time", () => {
    renderLevels();
    const black = screen.getByRole("button", { name: "Black point picker" });
    const white = screen.getByRole("button", { name: "White point picker" });
    fireEvent.click(black);
    expect(useEditorStore.getState().tatTarget).toBe("levelsBlack");
    fireEvent.click(white);
    expect(useEditorStore.getState().tatTarget).toBe("levelsWhite");
    fireEvent.click(white);
    expect(useEditorStore.getState().tatTarget).toBeNull();
  });
});

describe("Levels input histogram", () => {
  it("asks once the recipe rests, never for Levels' own edits or during drags", async () => {
    vi.useFakeTimers();
    try {
      const counts = [0, 1, 2].map(() => {
        const c = new Array(256).fill(0);
        c[64] = 50;
        return c;
      });
      invoke.mockResolvedValue(counts);
      const asked = () =>
        invoke.mock.calls.filter(([name]) => name === "levels_input_histogram").length;
      renderLevels();
      expect(asked()).toBe(0);
      await act(async () => vi.advanceTimersByTime(LEVELS_HISTOGRAM_DELAY_MS));
      expect(asked()).toBe(1);
      expect(invoke).toHaveBeenCalledWith("levels_input_histogram", {
        path: "/s/a.NEF",
        adjustments: useEditorStore.getState().adjustments,
      });
      // Drawn from the counts: a peak at a quarter.
      expect(document.querySelector(".histogram-red")?.getAttribute("d")).toContain("L64.25098039215686 2 ");
      const set = (patch: Partial<Adjustments>) =>
        act(async () => {
          useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, ...patch } }));
          vi.advanceTimersByTime(LEVELS_HISTOGRAM_DELAY_MS);
        });
      await set({ levels: { ...INITIAL_LEVELS, rgb: { ...INITIAL_LEVELS.rgb, inBlack: 30 } } });
      expect(asked()).toBe(1);
      useEditorStore.setState({ isSliderDragging: true });
      await set({ exposure: 0.3 });
      expect(asked()).toBe(1);
      await act(async () => {
        useEditorStore.setState({ isSliderDragging: false });
        vi.advanceTimersByTime(LEVELS_HISTOGRAM_DELAY_MS - 1);
      });
      expect(asked()).toBe(1);
      await act(async () => vi.advanceTimersByTime(1));
      expect(asked()).toBe(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("asks again when the full decode replaces the proxy, dropping the proxy's late answer", async () => {
    vi.useFakeTimers();
    try {
      const peak = (at: number) =>
        [0, 1, 2].map(() => {
          const c = new Array(256).fill(0);
          c[at] = 50;
          return c;
        });
      const answers: ((counts: number[][]) => void)[] = [];
      invoke.mockImplementation(
        () => new Promise<number[][]>((resolve) => answers.push(resolve)),
      );
      const asked = () =>
        invoke.mock.calls.filter(([name]) => name === "levels_input_histogram").length;
      const red = () => document.querySelector(".histogram-red")?.getAttribute("d") ?? "";
      const swap = (image: object) =>
        act(async () => {
          useEditorStore.setState((s) => ({
            selectedImage: { ...s.selectedImage!, ...image },
          }));
        });
      const rest = () => act(async () => vi.advanceTimersByTime(LEVELS_HISTOGRAM_DELAY_MS));
      renderLevels();
      await act(async () => vi.advanceTimersByTime(LEVELS_HISTOGRAM_DELAY_MS));
      expect(asked()).toBe(1);
      // An answered proxy: the full decode (same path and recipe) asks again.
      await act(async () => answers[0](peak(64)));
      expect(red()).toContain("L64.25098039215686 2 ");
      await swap({ isPreviewReady: true });
      await rest();
      expect(asked()).toBe(2);
      // Before that answer the source is replaced again: the answer from the
      // earlier source arrives late and is dropped.
      await swap({ isDecoded: true });
      await act(async () => answers[1](peak(32)));
      expect(red()).toContain("L64.25098039215686 2 ");
      await rest();
      expect(asked()).toBe(3);
      await act(async () => answers[2](peak(128)));
      expect(red()).not.toContain("L64.25098039215686 2 ");
      expect(red()).toContain("L128.");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("Levels picker on the photo", () => {
  it("samples what Levels receives and sets the active channel's point in one edit", async () => {
    const adjustments = structuredClone(INITIAL_ADJUSTMENTS);
    adjustments.levels!.rgb.inWhite = 200;
    useEditorStore.setState({
      adjustments,
      tatTarget: "levelsBlack",
      levelsChannel: "rgb",
    });
    const grey = linearFromSrgb([30, 30, 30]);
    invoke.mockResolvedValue([grey, grey]);
    render(<TargetedAdjustmentOverlay rect={{ left: 0, top: 0, width: 400, height: 300 }} />);
    const el = screen.getByTestId("targeted-adjustment");
    el.getBoundingClientRect = () => ({ left: 0, top: 0, width: 400, height: 300 }) as DOMRect;
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 100, clientY: 150, pointerId: 1 });
    });
    fireEvent.pointerUp(el, { pointerId: 1 });
    expect(invoke).toHaveBeenCalledWith("sample_curve_input", {
      path: "/s/a.NEF",
      normalizedX: 0.25,
      normalizedY: 0.5,
      adjustments: { ...adjustments, levels: INITIAL_LEVELS },
    });
    const state = useEditorStore.getState();
    expect(state.adjustments.levels?.rgb).toMatchObject({ inBlack: 30, inWhite: 200 });
    expect(state.tatTarget).toBeNull();
    expect(state.isSliderDragging).toBe(false);
  });

  it("cancels when the channel tab changes while the photo is sampled", async () => {
    useEditorStore.setState({ tatTarget: "levelsWhite", levelsChannel: "red" });
    let answer: (pixels: unknown) => void = () => {};
    invoke.mockReturnValue(new Promise((resolve) => (answer = resolve)));
    render(<TargetedAdjustmentOverlay rect={{ left: 0, top: 0, width: 400, height: 300 }} />);
    const el = screen.getByTestId("targeted-adjustment");
    el.getBoundingClientRect = () => ({ left: 0, top: 0, width: 400, height: 300 }) as DOMRect;
    fireEvent.pointerDown(el, { button: 0, clientX: 10, clientY: 10, pointerId: 1 });
    useEditorStore.setState({ levelsChannel: "green" });
    await act(async () => answer([linearFromSrgb([200, 200, 200])]));
    expect(useEditorStore.getState().adjustments.levels).toEqual(INITIAL_LEVELS);
  });

  it("leaves the levels alone without a sample", async () => {
    useEditorStore.setState({ tatTarget: "levelsWhite", levelsChannel: "red" });
    invoke.mockResolvedValue(null);
    render(<TargetedAdjustmentOverlay rect={{ left: 0, top: 0, width: 400, height: 300 }} />);
    const el = screen.getByTestId("targeted-adjustment");
    el.getBoundingClientRect = () => ({ left: 0, top: 0, width: 400, height: 300 }) as DOMRect;
    await act(async () => {
      fireEvent.pointerDown(el, { button: 0, clientX: 10, clientY: 10, pointerId: 1 });
    });
    expect(useEditorStore.getState().adjustments.levels).toEqual(INITIAL_LEVELS);
  });
});
