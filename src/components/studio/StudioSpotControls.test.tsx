import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));

import StudioSpotControls from "./StudioSpotControls";
import { toggleRemoveObjects } from "./useStudioTools";
import { useEditorStore } from "../../store/useEditorStore";
import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";
import { KEYBIND_DEFINITIONS } from "../../utils/keyboardUtils";

beforeEach(() => {
  useEditorStore.setState({
    selectedImage: { path: "/s/a.NEF", isReady: true, isRaw: true } as never,
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    isSpotToolActive: true,
    activeSpotId: null,
    spotSettings: { mode: "heal", size: 60, feather: 0, opacity: 100 },
    spotVisualize: { on: false, sensitivity: 50 },
  });
});
afterEach(cleanup);

describe("spot removal settings", () => {
  it("offers Detect objects in Remove only, with a brush (and its size) or a box", () => {
    render(<StudioSpotControls />);
    expect(screen.queryByLabelText("spotRemoval.detectObjects")).toBeNull();
    expect(screen.getByLabelText("spotRemoval.featherAria")).toBeTruthy();
    fireEvent.click(screen.getByRole("radio", { name: "spotRemoval.modes.remove" }));
    // Feather and opacity do not apply to Remove.
    expect(screen.queryByLabelText("spotRemoval.featherAria")).toBeNull();
    expect(screen.getByLabelText("spotRemoval.sizeAria")).toBeTruthy();
    fireEvent.click(screen.getByLabelText("spotRemoval.detectObjects"));
    expect(useEditorStore.getState().spotSettings).toMatchObject({ mode: "remove", detectObjects: true });
    // Brushing over the object is the default: its size stays.
    expect(screen.getByRole("radio", { name: "spotRemoval.objectSelectModes.brush" }).getAttribute("aria-checked")).toBe(
      "true",
    );
    expect(screen.getByLabelText("spotRemoval.sizeAria")).toBeTruthy();
    expect(screen.getByText(/spotRemoval\.detectObjectsHint/)).toBeTruthy();
    expect(screen.getByText(/spotRemoval\.detectObjectsRefineHint/)).toBeTruthy();
    fireEvent.click(screen.getByRole("radio", { name: "spotRemoval.objectSelectModes.box" }));
    expect(useEditorStore.getState().spotSettings.objectSelect).toBe("box");
    expect(screen.queryByLabelText("spotRemoval.sizeAria")).toBeNull();
    expect(screen.getByText(/spotRemoval\.detectObjectsBoxHint/)).toBeTruthy();
  });
});

describe("Shift+Q (remove objects)", () => {
  it("is a tools keybind of its own", () => {
    const def = KEYBIND_DEFINITIONS.find((d) => d.action === "remove_objects");
    expect(def).toMatchObject({ defaultCombo: ["shift", "KeyQ"], section: "tools" });
    const combos = KEYBIND_DEFINITIONS.flatMap((d) => [d.defaultCombo, ...(d.aliasCombos ?? [])])
      .filter((c) => c.length)
      .map((c) => c.join("+"));
    expect(combos.filter((c) => c === "shift+KeyQ")).toHaveLength(1);
  });

  it("opens the spot tool in Remove with Detect objects, and ends it when pressed again", () => {
    const selectTool = vi.fn((tool: string) => {
      if (tool === "spot")
        useEditorStore.setState({ isSpotToolActive: !useEditorStore.getState().isSpotToolActive });
    });
    useEditorStore.setState({ isSpotToolActive: false });
    toggleRemoveObjects(selectTool);
    expect(selectTool).toHaveBeenCalledWith("spot");
    expect(useEditorStore.getState()).toMatchObject({
      isSpotToolActive: true,
      spotSettings: { mode: "remove", detectObjects: true, size: 60 },
    });
    toggleRemoveObjects(selectTool);
    expect(useEditorStore.getState().isSpotToolActive).toBe(false);
    // With the tool on in Heal, it switches the mode without ending the tool.
    useEditorStore.setState({ isSpotToolActive: true, spotSettings: { mode: "heal", size: 60, feather: 0, opacity: 100 } });
    selectTool.mockClear();
    toggleRemoveObjects(selectTool);
    expect(selectTool).not.toHaveBeenCalled();
    expect(useEditorStore.getState()).toMatchObject({
      isSpotToolActive: true,
      spotSettings: { mode: "remove", detectObjects: true },
    });
  });
});
