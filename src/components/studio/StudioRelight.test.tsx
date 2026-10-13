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
    geometry: {
      transformRotate: 0,
      guidedPerspective: { autoCrop: true, enabled: false, lines: [] },
    },
  };
  vi.mocked(invoke).mockResolvedValue(depth);
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Analyze depth" }));
  await screen.findByText("Depth ready. Light follows the surface.");
  const reloaded = normalizeLoadedAdjustments(
    JSON.parse(JSON.stringify(useEditorStore.getState().adjustments)),
  );
  expect(reloaded.relightDepth).toEqual(depth);
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
  await screen.findByText("Depth ready. Light follows the surface.");
  fireEvent.click(screen.getByRole("button", { name: "Add light" }));
  expect(vi.mocked(invoke)).toHaveBeenCalledTimes(1);
});
