import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

import StudioLut from "./StudioLut";
import { useEditorStore } from "../../store/useEditorStore";
import { debouncedSetHistory } from "../../hooks/useEditorActions";
import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";
import type { SelectedImage } from "../ui/AppProperties";

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((name: string) =>
    Promise.resolve(
      name === "list_luts"
        ? [
            { name: "Warm 200", path: "/r/Warm 200.cube", isBuiltIn: true },
            { name: "Mine", path: "/u/Mine.cube", isBuiltIn: false },
          ]
        : name === "load_and_parse_lut"
          ? { size: 33 }
          : undefined,
    ),
  );
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    selectedImage: { path: "/s/a.NEF", isRaw: true, isReady: true } as SelectedImage,
  });
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
});

it("applies a LUT with its amount and removes it again", async () => {
  await act(async () => {
    render(<StudioLut disabled={false} onInteraction={vi.fn()} />);
  });
  const select = screen.getByRole("combobox", { name: "LUT" });
  expect([...select.querySelectorAll("option")].map((o) => o.textContent)).toEqual(["None", "Warm 200", "Mine"]);
  await act(async () => {
    fireEvent.change(select, { target: { value: "/u/Mine.cube" } });
  });
  expect(invoke).toHaveBeenCalledWith("load_and_parse_lut", { path: "/u/Mine.cube" });
  expect(useEditorStore.getState().adjustments).toMatchObject({ lutPath: "/u/Mine.cube", lutSize: 33 });
  const amount = screen.getByRole("textbox", { name: "LUT amount value" });
  fireEvent.focus(amount);
  fireEvent.change(amount, { target: { value: "40" } });
  fireEvent.blur(amount);
  expect(useEditorStore.getState().adjustments.lutIntensity).toBe(40);
  fireEvent.change(select, { target: { value: "" } });
  expect(useEditorStore.getState().adjustments).toMatchObject({ lutPath: null, lutIntensity: 100 });
});

it("removes an imported LUT, not a built-in one", async () => {
  useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, lutPath: "/r/Warm 200.cube" } }));
  await act(async () => {
    render(<StudioLut disabled={false} onInteraction={vi.fn()} />);
  });
  expect(screen.queryByRole("button", { name: "Remove" })).toBeNull();
  useEditorStore.setState((s) => ({ adjustments: { ...s.adjustments, lutPath: "/u/Mine.cube" } }));
  await act(async () => {
    fireEvent.click(await screen.findByRole("button", { name: "Remove" }));
  });
  expect(invoke).toHaveBeenCalledWith("remove_lut", { path: "/u/Mine.cube" });
  expect(useEditorStore.getState().adjustments.lutPath).toBeNull();
});
