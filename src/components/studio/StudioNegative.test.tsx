import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

const { invoke, toast } = vi.hoisted(() => ({
  invoke: vi.fn(),
  toast: { warn: vi.fn(), info: vi.fn(), error: vi.fn() },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("react-toastify", () => ({ toast }));

import StudioNegative, { baseSwatch } from "./StudioNegative";
import { useEditorStore } from "../../store/useEditorStore";
import { debouncedSetHistory } from "../../hooks/useEditorActions";
import { INITIAL_ADJUSTMENTS, NEGATIVE_VERSION, newNegative } from "../../utils/adjustments";
import { cancelNegativeAnalysis } from "../../utils/negative";
import type { SelectedImage } from "../ui/AppProperties";

const ANALYSIS = { base: [0.7, 0.42, 0.2], highlight: [0.06, 0.02, 0.008], baseFromBorder: true };

beforeEach(() => {
  invoke.mockReset();
  Object.values(toast).forEach((f) => f.mockReset());
  invoke.mockImplementation((name: string) =>
    Promise.resolve(name === "analyze_negative" ? ANALYSIS : undefined),
  );
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    selectedImage: { path: "/s/neg.NEF", isRaw: true, isReady: true } as SelectedImage,
  });
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
});

it("leaves recipes without the tool untouched", () => {
  // Off is no key at all, so such recipes render as before.
  expect("negative" in INITIAL_ADJUSTMENTS).toBe(false);
});

it("turns a colour or black-and-white negative on from the photo's analysis", async () => {
  // The crop is the frame the analysis reads.
  const crop = { x: 10, y: 20, width: 300, height: 200, unit: "px" as const };
  useEditorStore.setState({ adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), crop } });
  render(<StudioNegative disabled={false} onInteraction={vi.fn()} />);
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "B&W negative" }));
  });
  expect(invoke).toHaveBeenCalledWith("analyze_negative", {
    path: "/s/neg.NEF",
    jsAdjustments: expect.objectContaining({ crop }),
  });
  expect(useEditorStore.getState().adjustments.negative).toEqual({
    version: NEGATIVE_VERSION,
    enabled: true,
    type: "bw",
    base: ANALYSIS.base,
    highlight: ANALYSIS.highlight,
    balance: [0, 0, 0],
    exposure: 0,
    grade: 0,
  });
});

it("keeps the print settings when the film base is found again", async () => {
  useEditorStore.setState({
    adjustments: {
      ...structuredClone(INITIAL_ADJUSTMENTS),
      negative: { ...newNegative([0.5, 0.5, 0.5], [0.1, 0.1, 0.1]), exposure: 0.7, balance: [0, 10, 0] },
    },
  });
  render(<StudioNegative disabled={false} onInteraction={vi.fn()} />);
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Find film base and highlights" }));
  });
  expect(useEditorStore.getState().adjustments.negative).toMatchObject({
    base: ANALYSIS.base,
    highlight: ANALYSIS.highlight,
    exposure: 0.7,
    balance: [0, 10, 0],
  });
});

it("drops an analysis that arrives after another photo was opened", async () => {
  invoke.mockImplementation(() => {
    useEditorStore.setState({ selectedImage: { path: "/s/other.NEF", isRaw: true, isReady: true } as SelectedImage });
    return Promise.resolve(ANALYSIS);
  });
  render(<StudioNegative disabled={false} onInteraction={vi.fn()} />);
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Color negative" }));
  });
  expect(useEditorStore.getState().adjustments.negative).toBeUndefined();
});

it("changes one channel's density balance and arms the film base picker", () => {
  useEditorStore.setState({
    adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), negative: newNegative([0.7, 0.4, 0.2], [0.1, 0.05, 0.02]) },
    isWbPickerActive: true,
  });
  render(<StudioNegative disabled={false} onInteraction={vi.fn()} />);
  const blue = screen.getByRole("textbox", { name: "Blue value" });
  fireEvent.focus(blue);
  fireEvent.change(blue, { target: { value: "25" } });
  fireEvent.blur(blue);
  expect(useEditorStore.getState().adjustments.negative?.balance).toEqual([0, 0, 25]);
  fireEvent.click(screen.getByRole("button", { name: /Pick the film base/ }));
  expect(useEditorStore.getState()).toMatchObject({ filmBasePicking: true, isWbPickerActive: false });
});

it("shows the film base as its colour, scaled to its brightest channel", () => {
  expect(baseSwatch([0.8, 0.4, 0.2])).toBe("rgb(255, 188, 137)");
});

it("keeps controls moved and a reset made while the analysis runs", async () => {
  let answer: (value: unknown) => void = () => {};
  invoke.mockImplementation(() => new Promise((resolve) => (answer = resolve)));
  useEditorStore.setState({
    adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), negative: newNegative([0.5, 0.5, 0.5], [0.1, 0.1, 0.1]) },
  });
  render(<StudioNegative disabled={false} onInteraction={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Find film base and highlights" }));
  // Print exposure moved while the analysis runs.
  act(() => {
    useEditorStore.setState((s) => ({
      adjustments: { ...s.adjustments, negative: { ...s.adjustments.negative!, exposure: 0.4 } },
    }));
  });
  await act(async () => answer(ANALYSIS));
  expect(useEditorStore.getState().adjustments.negative).toMatchObject({ exposure: 0.4, base: ANALYSIS.base });

  // A reset during the analysis stays reset.
  fireEvent.click(screen.getByRole("button", { name: "Find film base and highlights" }));
  act(() => {
    useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, negative: undefined } }));
  });
  await act(async () => answer(ANALYSIS));
  expect(useEditorStore.getState().adjustments.negative).toBeUndefined();
});

it("drops a first conversion's analysis when the tool is reset meanwhile", async () => {
  let answer: (value: unknown) => void = () => {};
  invoke.mockImplementation(() => new Promise((resolve) => (answer = resolve)));
  render(<StudioNegative disabled={false} onInteraction={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Color negative" }));
  // The tool's reset (StudioDevelop's onReset) while the analysis runs.
  act(() => cancelNegativeAnalysis());
  await act(async () => answer(ANALYSIS));
  expect(useEditorStore.getState().adjustments.negative).toBeUndefined();
});

it("drops a first conversion's analysis when the photo's adjustments are reset meanwhile", async () => {
  let answer: (value: unknown) => void = () => {};
  invoke.mockImplementation(() => new Promise((resolve) => (answer = resolve)));
  render(<StudioNegative disabled={false} onInteraction={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "B&W negative" }));
  // Reset Adjustments replaces the recipe while the analysis runs.
  act(() => useEditorStore.setState({ adjustments: structuredClone(INITIAL_ADJUSTMENTS) }));
  await act(async () => answer(ANALYSIS));
  expect(useEditorStore.getState().adjustments.negative).toBeUndefined();
});
