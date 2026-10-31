import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invoke, sync, update } = vi.hoisted(() => ({ invoke: vi.fn(), sync: vi.fn(), update: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, unknown>) => (values ? `${key} ${JSON.stringify(values)}` : key),
  }),
}));
vi.mock("../../hooks/useEditorActions", async (original) => {
  const actual = await original<typeof import("../../hooks/useEditorActions")>();
  update.mockImplementation(actual.updateAdjustments);
  return { ...actual, updateAdjustments: update, syncDustToPaths: sync };
});

import { StudioFindDust } from "./StudioSpotControls";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { INITIAL_ADJUSTMENTS, type Spot } from "../../utils/adjustments";

const found = (n: number) =>
  Array.from({ length: n }, (_, i) => ({ x: 100 + 50 * i, y: 80, radius: 12 }));
const healed = (n: number) =>
  found(n).map((s, i) => ({ ...s, sourceX: s.x + 60, sourceY: s.y, visible: true, dust: { x: 0.1 * i, y: 0.2 } }));

beforeEach(() => {
  vi.useFakeTimers();
  invoke.mockReset();
  sync.mockReset();
  update.mockClear();
  useEditorStore.setState({
    selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    isSpotToolActive: true,
    activeSpotId: null,
    spotSettings: { mode: "heal", size: 60, feather: 20, opacity: 100 },
    spotVisualize: { on: false, sensitivity: 50 },
    dustPreview: null,
  });
  useLibraryStore.setState({ multiSelectedPaths: [] });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

/** Lets the debounced preview request run and answer. */
const settle = async () => {
  await act(async () => {
    vi.advanceTimersByTime(200);
  });
  await act(async () => {});
};

describe("Find Dust", () => {
  it("previews the spots found and follows the sensitivity live", async () => {
    invoke.mockImplementation((_cmd: string, args: { sensitivity: number }) =>
      Promise.resolve({ spots: found(args.sensitivity > 60 ? 3 : 2), facesChecked: true }),
    );
    render(<StudioFindDust />);
    fireEvent.click(screen.getByText("spotRemoval.dust.find"));
    await settle();
    expect(invoke).toHaveBeenCalledWith("find_dust", expect.objectContaining({ sensitivity: 50, apply: false }));
    expect(useEditorStore.getState().dustPreview?.spots).toHaveLength(2);
    expect(screen.getByText(/spotRemoval\.dust\.found \{"number":2\}/)).toBeTruthy();
    // The slider is Visualize Spots' sensitivity.
    act(() => useEditorStore.setState({ spotVisualize: { on: false, sensitivity: 80 } }));
    await settle();
    expect(invoke).toHaveBeenLastCalledWith("find_dust", expect.objectContaining({ sensitivity: 80, apply: false }));
    expect(useEditorStore.getState().dustPreview?.spots).toHaveLength(3);
    // Nothing is in the recipe before Apply.
    expect(useEditorStore.getState().adjustments.spots ?? []).toHaveLength(0);
  });

  it("applies every spot as an ordinary heal spot in one undo step and ends the preview", async () => {
    invoke.mockImplementation((_cmd: string, args: { apply: boolean }) =>
      Promise.resolve({ spots: args.apply ? healed(3) : found(3), facesChecked: true }),
    );
    const existing: Spot = {
      id: "own", mode: "clone", x: 10, y: 10, radius: 5, sourceX: 30, sourceY: 10, feather: 0, opacity: 100, visible: true,
    };
    useEditorStore.setState({ adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), spots: [existing] } });
    render(<StudioFindDust />);
    fireEvent.click(screen.getByText("spotRemoval.dust.find"));
    await settle();
    await act(async () => {
      fireEvent.click(screen.getByText("spotRemoval.dust.apply"));
    });
    await act(async () => {});
    expect(invoke).toHaveBeenLastCalledWith("find_dust", expect.objectContaining({ apply: true }));
    expect(update).toHaveBeenCalledTimes(1);
    const spots = useEditorStore.getState().adjustments.spots!;
    expect(spots).toHaveLength(4);
    expect(spots[0]).toEqual(existing);
    for (const spot of spots.slice(1)) {
      expect(spot).toMatchObject({ mode: "heal", feather: 20, opacity: 100, visible: true });
      expect(spot.id).toBeTruthy();
      expect(spot.dust).toBeTruthy();
    }
    expect(new Set(spots.map((s) => s.id)).size).toBe(4);
    expect(useEditorStore.getState().dustPreview).toBeNull();
    expect(screen.getByText(/spotRemoval\.dust\.applied \{"number":3\}/)).toBeTruthy();
  });

  it("adds nothing when the run finds nothing new (no duplicates)", async () => {
    invoke.mockResolvedValue({ spots: [], facesChecked: false });
    render(<StudioFindDust />);
    fireEvent.click(screen.getByText("spotRemoval.dust.find"));
    await settle();
    const apply = screen.getByText("spotRemoval.dust.apply") as HTMLButtonElement;
    expect(apply.disabled).toBe(true);
    // Without the face models the user is told faces were not left out.
    expect(screen.getByText(/spotRemoval\.dust\.facesNotChecked/)).toBeTruthy();
    fireEvent.click(screen.getByText("spotRemoval.cancel"));
    expect(useEditorStore.getState().dustPreview).toBeNull();
    expect(update).not.toHaveBeenCalled();
  });

  it("ends the preview in Remove, in Red eye and when the tool ends", async () => {
    invoke.mockResolvedValue({ spots: found(1), facesChecked: true });
    render(<StudioFindDust />);
    fireEvent.click(screen.getByText("spotRemoval.dust.find"));
    await settle();
    expect(useEditorStore.getState().dustPreview).not.toBeNull();
    act(() => useEditorStore.setState({ isSpotToolActive: false }));
    expect(useEditorStore.getState().dustPreview).toBeNull();
    act(() => useEditorStore.setState({ isSpotToolActive: true }));
    fireEvent.click(screen.getByText("spotRemoval.dust.find"));
    await settle();
    act(() => useEditorStore.setState({ spotSettings: { mode: "remove", size: 60, feather: 0, opacity: 100 } }));
    expect(useEditorStore.getState().dustPreview).toBeNull();
    expect(screen.queryByText("spotRemoval.dust.find")).toBeNull();
  });

  it("syncs the dust to the other selected photos and names the skipped ones", async () => {
    const dusty: Spot = {
      id: "d", mode: "heal", x: 100, y: 80, radius: 12, sourceX: 160, sourceY: 80, feather: 0, opacity: 100,
      visible: true, dust: { x: 0.1, y: 0.2 },
    };
    useEditorStore.setState({ adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), spots: [dusty] } });
    // Only with dust spots and other selected photos.
    render(<StudioFindDust />);
    expect(screen.queryByText(/spotRemoval\.dust\.sync /)).toBeNull();
    cleanup();
    useLibraryStore.setState({ multiSelectedPaths: ["/s/a.NEF", "/s/b.NEF", "/s/c.NEF", "/s/d.CR3"] });
    sync.mockResolvedValue({
      failed: [],
      skipped: [
        { path: "/s/d.CR3", reason: "otherCamera" },
        { path: "/s/c.NEF", reason: "otherFrame" },
      ],
    });
    render(<StudioFindDust />);
    await act(async () => {
      fireEvent.click(screen.getByText(/spotRemoval\.dust\.sync \{"number":3\}/));
    });
    expect(sync).toHaveBeenCalledWith("/s/a.NEF", ["/s/b.NEF", "/s/c.NEF", "/s/d.CR3"], expect.objectContaining({ spots: [dusty] }));
    expect(screen.getByText(/spotRemoval\.dust\.synced \{"number":1\}/)).toBeTruthy();
    expect(screen.getByText(/skippedOtherFrame \{"names":"c.NEF"\}/)).toBeTruthy();
    expect(screen.getByText(/skippedOtherCamera \{"names":"d.CR3"\}/)).toBeTruthy();
  });
});
