import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import StudioUpright from "./StudioUpright";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { INITIAL_ADJUSTMENTS, type Adjustments } from "../../utils/adjustments";
import { Panel } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const setAdjustments = vi.fn((update: Adjustments | ((prev: Adjustments) => Adjustments)) => {
  const prev = useEditorStore.getState().adjustments;
  useEditorStore.setState({ adjustments: typeof update === "function" ? update(prev) : update });
});
vi.mock("../../hooks/useEditorActions", () => ({
  useEditorActions: () => ({ setAdjustments }),
  isCurrentEditorImage: (path: string) => {
    const image = useEditorStore.getState().selectedImage;
    return image?.path === path && image.isReady;
  },
}));
const mockInvoke = vi.mocked(invoke);
const lines = [
  { id: "vertical-0", type: "vertical", p1: { x: 0.3, y: 0.2 }, p2: { x: 0.31, y: 0.8 } },
  { id: "vertical-1", type: "vertical", p1: { x: 0.7, y: 0.2 }, p2: { x: 0.69, y: 0.8 } },
];

beforeEach(() => {
  mockInvoke.mockReset();
  setAdjustments.mockClear();
  useEditorStore.setState({
    selectedImage: { path: "/a.raf", isReady: true } as never,
    adjustments: { ...INITIAL_ADJUSTMENTS },
    isGuidedPerspectiveActive: false,
  });
  useUIStore.setState({ activePanel: Panel.Adjustments });
});
afterEach(cleanup);

const guided = () => useEditorStore.getState().adjustments.guidedPerspective;

describe("Upright", () => {
  it("applies the lines found for an automatic mode", async () => {
    mockInvoke.mockResolvedValue({ lines, applied: "vertical", segments: 40, agreeing: 9 });
    render(<StudioUpright />);
    await act(async () => {
      fireEvent.click(screen.getByText("Auto"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("calculate_upright", expect.objectContaining({ mode: "auto" }));
    expect(guided()).toEqual({ enabled: true, lines, autoCrop: true, upright: "auto" });
    expect(screen.getByText("Auto: converging verticals made parallel.")).toBeTruthy();
    expect(screen.getByText("Auto").getAttribute("aria-checked")).toBe("true");
    fireEvent.click(screen.getByText("Off"));
    expect(guided()).toEqual({ enabled: false, lines: [], autoCrop: true, upright: "off" });
  });

  it("explains when no lines were found", async () => {
    mockInvoke.mockResolvedValue(null);
    render(<StudioUpright />);
    await act(async () => {
      fireEvent.click(screen.getByText("Vertical"));
    });
    expect(screen.getByText(/try Level or Guided/)).toBeTruthy();
    expect(guided().enabled).toBe(false);
  });

  it("draws guided lines on the uncropped photo", () => {
    render(<StudioUpright />);
    fireEvent.click(screen.getByText("Guided"));
    expect(useEditorStore.getState().isGuidedPerspectiveActive).toBe(true);
    expect(useUIStore.getState().activePanel).toBe(Panel.Crop);
    expect(guided().upright).toBe("guided");
    fireEvent.click(screen.getByText("Done"));
    expect(useEditorStore.getState().isGuidedPerspectiveActive).toBe(false);
    // Still in Crop & geometry (the crop panel).
    expect(useUIStore.getState().activePanel).toBe(Panel.Crop);
  });

  it("counts guides per axis, clears them and keeps Constrain crop", () => {
    useEditorStore.setState({
      adjustments: {
        ...INITIAL_ADJUSTMENTS,
        guidedPerspective: { enabled: true, lines, autoCrop: true, upright: "guided" },
      } as Adjustments,
    });
    render(<StudioUpright />);
    expect(screen.getByText("2 vertical · 0 horizontal")).toBeTruthy();
    expect(screen.getByText("Corrected along your guides.")).toBeTruthy();
    fireEvent.click(screen.getByRole("checkbox", { name: "Constrain crop" }));
    expect(guided().autoCrop).toBe(false);
    fireEvent.click(screen.getByText("Clear"));
    expect(guided()).toEqual({ enabled: false, lines: [], autoCrop: false, upright: "guided" });
    // Re-entering Guided keeps the drawn guides; another mode keeps the crop choice.
    fireEvent.click(screen.getByText("Edit guides"));
    expect(useEditorStore.getState().isGuidedPerspectiveActive).toBe(true);
    fireEvent.click(screen.getByText("Off"));
    expect(guided().autoCrop).toBe(false);
  });

  it("drops lines that arrive after another photograph opened", async () => {
    let finish!: (value: unknown) => void;
    mockInvoke.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    render(<StudioUpright />);
    fireEvent.click(screen.getByText("Auto"));
    const next = { ...INITIAL_ADJUSTMENTS };
    act(() => useEditorStore.setState({ selectedImage: { path: "/b.raf", isReady: true } as never, adjustments: next }));
    await act(async () => finish({ lines, applied: "vertical", segments: 40, agreeing: 9 }));
    expect(useEditorStore.getState().adjustments).toBe(next);
    expect(setAdjustments).not.toHaveBeenCalled();
  });
});
