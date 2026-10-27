import "../../i18n";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import StudioRelight from "./StudioRelight";
import { useRelightStore } from "../../store/useRelightStore";
import { useEditorStore } from "../../store/useEditorStore";
import {
  INITIAL_ADJUSTMENTS,
  normalizeLoadedAdjustments,
} from "../../utils/adjustments";
import { createRelightLight, type RelightDepth } from "../../utils/relight";
import { debouncedSetHistory } from "../../hooks/useEditorActions";
import type { SelectedImage } from "../ui/AppProperties";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
import { open } from "@tauri-apps/plugin-dialog";
beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useRelightStore.setState(useRelightStore.getInitialState());
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    selectedImage: {
      path: "/qa/photo.jpg",
      width: 1000,
      height: 800,
      isReady: true,
      isRaw: false,
    } as SelectedImage,
    adjustments: {
      ...structuredClone(INITIAL_ADJUSTMENTS),
      relightLights: [createRelightLight()],
    },
  });
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
});
const mount = () =>
  render(<StudioRelight disabled={false} onInteraction={() => {}} />);
it("adds up to four lights, toggles and removes them without altering other edits", () => {
  mount();
  for (let i = 0; i < 3; i++)
    fireEvent.click(screen.getByRole("button", { name: "Add light" }));
  expect(
    (screen.getByRole("button", { name: "Add light" }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Light enabled" }));
  expect(useEditorStore.getState().adjustments.relightLights?.[3].enabled).toBe(
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "Remove light" }));
  expect(useEditorStore.getState().adjustments.relightLights).toHaveLength(3);
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
});
it("stores depth once and preserves it through a recipe reload", async () => {
  const depth = {
    data: "data:image/png;base64,test",
    version: 2,
    faces: [[[0.5, 0.5, 0.02, 1]]],
    geometry: {
      transformRotate: 0,
      guidedPerspective: { autoCrop: true, enabled: false, lines: [] },
    },
  };
  vi.mocked(invoke).mockResolvedValue(depth);
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Analyze depth" }));
  await screen.findByText(
    "Estimated depth ready. Reduce surface strength if edges look unnatural.",
  );
  expect(screen.getByText("Faces refined: 1")).toBeTruthy();
  const reloaded = normalizeLoadedAdjustments(
    JSON.parse(JSON.stringify(useEditorStore.getState().adjustments)),
  );
  expect(reloaded.relightDepth).toEqual({
    ...depth,
    cacheKey: useEditorStore.getState().adjustments.relightDepth!.cacheKey,
  });
  fireEvent.change(screen.getByLabelText("Light color"), {
    target: { value: "#ff8800" },
  });
  expect(vi.mocked(invoke)).toHaveBeenCalledTimes(1);
  expect(useEditorStore.getState().adjustments.relightLights?.[0].color).toBe(
    "#ff8800",
  );
});
it("rejects analysis that finishes after switching photographs", async () => {
  let resolve!: (v: RelightDepth) => void;
  vi.mocked(invoke).mockReturnValue(
    new Promise<RelightDepth>((r) => {
      resolve = r;
    }),
  );
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Analyze depth" }));
  act(() =>
    useEditorStore.setState({
      selectedImage: {
        ...useEditorStore.getState().selectedImage!,
        path: "/qa/other.jpg",
      },
    }),
  );
  await act(async () => resolve({ data: "old", geometry: {} }));
  expect(useEditorStore.getState().adjustments.relightDepth).toBeUndefined();
});
it("shows errors and allows retry; geometry edits invalidate the analysis", async () => {
  vi.mocked(invoke).mockRejectedValue(new Error("model unavailable"));
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Analyze depth" }));
  await screen.findByRole("alert");
  await waitFor(() =>
    expect(
      (
        screen.getByRole("button", {
          name: "Analyze depth",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false),
  );
  act(() =>
    useEditorStore.setState((s) => ({
      adjustments: {
        ...s.adjustments,
        relightDepth: { data: "old", geometry: { transformRotate: 15 } },
      },
    })),
  );
  expect(
    screen.getByText("Geometry changed. Analyze depth again."),
  ).toBeTruthy();
});

it("selects a visual pattern and shares light selection with the canvas", () => {
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Pattern" }));
  fireEvent.click(screen.getByRole("button", { name: "Palm" }));
  expect(useEditorStore.getState().adjustments.relightLights![0].pattern).toBe(
    "palm",
  );
  expect(screen.queryByRole("group", { name: "Pattern" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Add light" }));
  const lights = useEditorStore.getState().adjustments.relightLights!;
  expect(useRelightStore.getState().selected).toBe(lights[1].id);
  act(() => useRelightStore.setState({ selected: lights[0].id }));
  expect(
    screen
      .getByRole("button", { name: "Light 1" })
      .getAttribute("aria-pressed"),
  ).toBe("true");
});
it("analyzes depth on adding a light, then reuses it for subsequent lights", async () => {
  useEditorStore.setState((s) => ({
    adjustments: { ...s.adjustments, relightLights: [] },
  }));
  vi.mocked(invoke).mockResolvedValue({ data: "depth", geometry: {} });
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Add light" }));
  await screen.findByText(
    "Estimated depth ready. Reduce surface strength if edges look unnatural.",
  );
  fireEvent.click(screen.getByRole("button", { name: "Add light" }));
  expect(vi.mocked(invoke)).toHaveBeenCalledTimes(1);
});

it("names an analysis, shows why it failed, and drops depth with the last light", async () => {
  vi.mocked(invoke).mockRejectedValueOnce("Face models: download failed (offline)");
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Analyze depth" }));
  expect((await screen.findByRole("alert")).textContent).toContain("download failed (offline)");
  vi.mocked(invoke).mockResolvedValueOnce({ data: "depth", geometry: {} });
  fireEvent.click(screen.getByRole("button", { name: "Analyze depth" }));
  await screen.findByText("Estimated depth ready. Reduce surface strength if edges look unnatural.");
  expect(useEditorStore.getState().adjustments.relightDepth?.cacheKey).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Remove light" }));
  expect(useEditorStore.getState().adjustments.relightDepth).toBeUndefined();
});

it("keeps light numbers, placement on reset, and solo outside the recipe", () => {
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Add light" }));
  fireEvent.click(screen.getByRole("button", { name: "Add light" }));
  act(() => useRelightStore.setState({ selected: useEditorStore.getState().adjustments.relightLights![0].id }));
  fireEvent.click(screen.getByRole("button", { name: "Remove light" }));
  expect(screen.queryByRole("button", { name: "Light 1" })).toBeNull();
  expect(screen.getByRole("button", { name: "Light 3" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Light 3" }));
  const before = useEditorStore.getState().adjustments.relightLights!.find((l) => l.number === 3)!;
  fireEvent.click(screen.getByRole("button", { name: "Reset light" }));
  const after = useEditorStore.getState().adjustments.relightLights!.find((l) => l.number === 3)!;
  expect([after.sourceX, after.sourceY, after.id]).toEqual([before.sourceX, before.sourceY, before.id]);
  const recipe = useEditorStore.getState().adjustments;
  fireEvent.click(screen.getByRole("button", { name: "Show only this light" }));
  expect(useRelightStore.getState().solo).toBe(after.id);
  expect(useEditorStore.getState().adjustments).toBe(recipe);
});

it("applies a preset, a color temperature and a mask limit", () => {
  useEditorStore.setState((s) => ({
    adjustments: {
      ...s.adjustments,
      relightDepth: { data: "d", geometry: {} },
      masks: [{ id: "m1", name: "Subject", visible: true, subMasks: [] } as never],
    },
  }));
  mount();
  fireEvent.change(screen.getByRole("combobox", { name: "Lighting preset" }), { target: { value: "butterfly" } });
  const [light] = useEditorStore.getState().adjustments.relightLights!;
  expect(light).toMatchObject({ rotation: 180, number: 1 });
  fireEvent.click(screen.getByRole("button", { name: "2700 K" }));
  expect(useEditorStore.getState().adjustments.relightLights![0]).toMatchObject({ temperature: 2700 });
  fireEvent.change(screen.getByRole("combobox", { name: "Limit to" }), { target: { value: "m1" } });
  expect(useEditorStore.getState().adjustments.relightLights![0].maskId).toBe("m1");
  // An empty layer switches the light off; the panel says so.
  expect(screen.getByText("This mask layer is hidden or empty, so the light is off.")).toBeTruthy();
});

it("places a new light in a corner no existing source is near", () => {
  mount();
  fireEvent.change(screen.getByRole("combobox", { name: "Lighting preset" }), { target: { value: "rembrandt" } });
  fireEvent.click(screen.getByRole("button", { name: "Add light" }));
  const [preset, added] = useEditorStore.getState().adjustments.relightLights!;
  // The preset's source sits near the top-left corner; the new light avoids it.
  expect(preset.sourceX).toBeLessThan(30);
  expect([added.sourceX, added.sourceY]).toEqual([80, 20]);
});

it("imports an image pattern through the backend", async () => {
  vi.mocked(invoke).mockResolvedValue("data:image/png;base64,PATTERN");
  vi.mocked(open).mockResolvedValue("/qa/leaves.png");
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Pattern" }));
  fireEvent.click(screen.getByRole("button", { name: "Import image…" }));
  await waitFor(() =>
    expect(useEditorStore.getState().adjustments.relightLights![0]).toMatchObject({
      pattern: "custom",
      customPattern: { data: "data:image/png;base64,PATTERN", name: "leaves.png" },
    }),
  );
  expect(vi.mocked(invoke)).toHaveBeenCalledWith("load_relight_pattern", { path: "/qa/leaves.png" });
});

it("hides and restores light guides without changing the lighting recipe", () => {
  mount();
  const recipe = useEditorStore.getState().adjustments;
  const checkbox = screen.getByRole("checkbox", {
    name: "Show light guides",
  }) as HTMLInputElement;
  expect(checkbox.checked).toBe(true);
  fireEvent.click(checkbox);
  expect(useRelightStore.getState().showHandles).toBe(false);
  expect(useEditorStore.getState().adjustments).toBe(recipe);
  fireEvent.click(checkbox);
  expect(useRelightStore.getState().showHandles).toBe(true);
  expect(useEditorStore.getState().adjustments).toBe(recipe);
});

it("defaults face reflections only for new analyses, edits and reloads without inference", async () => {
  vi.mocked(invoke).mockResolvedValue({
    data: "data:image/png;base64,test",
    version: 3,
    faces: [[[0.5, 0.5, 0.02, 1]]],
    geometry: {
      transformRotate: 0,
      guidedPerspective: { autoCrop: true, enabled: false, lines: [] },
    },
  });
  mount();
  expect(screen.queryByText("Face reflections")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Analyze depth" }));
  await screen.findByText("Faces refined: 1");
  fireEvent.click(screen.getByText("Advanced"));
  const input = screen.getByRole("textbox", { name: "Face reflections value" });
  expect((input as HTMLInputElement).value).toBe("25");
  fireEvent.change(input, { target: { value: "65" } });
  fireEvent.blur(input);
  await waitFor(() =>
    expect(useEditorStore.getState().adjustments.relightSkinReflections).toBe(
      65,
    ),
  );
  const loaded = normalizeLoadedAdjustments(
    JSON.parse(JSON.stringify(useEditorStore.getState().adjustments)),
  );
  expect(loaded.relightSkinReflections).toBe(65);
  expect(vi.mocked(invoke)).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Remove depth" }));
  expect(screen.queryByText("Face reflections")).toBeNull();
});
it("double-click resets every slider to the value a new light starts with", () => {
  const fresh = createRelightLight();
  // Old recipe: no scene values, settings far from a new light's.
  useEditorStore.setState({
    adjustments: {
      ...structuredClone(INITIAL_ADJUSTMENTS),
      relightDepth: { data: "d", geometry: {}, version: 5, faces: [[[0.5, 0.5, 0.02, 1]]] },
      relightSkinReflections: 80,
      relightLights: [
        { ...fresh, pattern: "blinds", intensity: 90, softness: 1, diffusion: 2, rotation: -10,
          elevation: 80, shadows: 3, scale: 200, patternRotation: 40, x: 10, y: 90 },
      ],
    },
  });
  mount();
  // Absent scene values show the renderer's defaults; reset gives the new-light start.
  expect((screen.getByRole("slider", { name: "Original light" }) as HTMLInputElement).value).toBe("100");
  const expected: [string, number][] = [
    ["Original light", 60],
    ["Surface strength", 100],
    ["Face reflections", 25],
    ["Intensity", fresh.intensity],
    ["Softness", fresh.softness],
    ["Diffusion", fresh.diffusion],
    ["Direction / rotation", fresh.rotation],
    ["Elevation", fresh.elevation],
    ["Shadow depth", fresh.shadows],
    ["Pattern scale", fresh.scale],
    ["Pattern rotation", fresh.patternRotation!],
    ["Position X", fresh.x],
    ["Position Y", fresh.y],
  ];
  for (const [name] of expected)
    fireEvent.doubleClick(screen.getByRole("slider", { name }));
  const after = useEditorStore.getState().adjustments;
  const [light] = after.relightLights!;
  const values: Record<string, number | undefined> = {
    "Original light": after.relightOriginalLight,
    "Surface strength": after.relightDepthAmount,
    "Face reflections": after.relightSkinReflections,
    Intensity: light.intensity,
    Softness: light.softness,
    Diffusion: light.diffusion,
    "Direction / rotation": light.rotation,
    Elevation: light.elevation,
    "Shadow depth": light.shadows,
    "Pattern scale": light.scale,
    "Pattern rotation": light.patternRotation,
    "Position X": light.x,
    "Position Y": light.y,
  };
  for (const [name, value] of expected) expect([name, values[name]]).toEqual([name, value]);
  fireEvent.change(screen.getByRole("combobox", { name: "Light type" }), { target: { value: "spot" } });
  fireEvent.doubleClick(screen.getByRole("slider", { name: "Light size" }));
  fireEvent.doubleClick(screen.getByRole("slider", { name: "Light distance" }));
  const spot = useEditorStore.getState().adjustments.relightLights![0];
  expect([spot.size, spot.distance]).toEqual([fresh.size, fresh.distance]);
});
