import "../../i18n";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";
import { useEditorStore } from "../../store/useEditorStore";
import { StudioBlackAndWhite, StudioColorGrading } from "./StudioColorTools";

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

const renderGrading = (
  colorGrading = INITIAL_ADJUSTMENTS.colorGrading,
  onInteraction: (active: boolean) => void = () => {},
) => {
  const change = vi.fn();
  render(
    <StudioColorGrading
      values={{
        colorGrading,
        sectionVisibility: INITIAL_ADJUSTMENTS.sectionVisibility,
      }}
      disabled={false}
      change={change}
      onInteraction={onInteraction}
    />,
  );
  const header = screen.queryByRole("button", { name: /Color grading/ });
  if (header && !screen.queryByText("Blending")) fireEvent.click(header);
  return change;
};

/** The colour grading the last change asked for. */
const lastGrading = (change: ReturnType<typeof vi.fn>) =>
  change.mock.calls.at(-1)![0].colorGrading;

describe("colour grading", () => {
  it("offers one wheel per range with Blending and Balance", () => {
    renderGrading();
    expect(screen.getByText("Blending")).toBeTruthy();
    expect(screen.getByText("Balance")).toBeTruthy();
    expect(screen.getByLabelText("Midtones luminance")).toBeTruthy();
  });

  it("edits the selected range only", () => {
    const change = renderGrading();
    fireEvent.click(screen.getByRole("button", { name: /Shadows/ }));
    expect(screen.getByLabelText("Shadows luminance")).toBeTruthy();
    expect(change).not.toHaveBeenCalled();
  });
});

describe("colour grading 3-way view", () => {
  it("opens with three wheels and luminance arcs above Blending and Balance", () => {
    renderGrading();
    expect(
      screen
        .getByRole("button", { name: "3-way" })
        .getAttribute("aria-pressed"),
    ).toBe("true");
    for (const range of ["Midtones", "Shadows", "Highlights"]) {
      expect(
        screen.getByRole("slider", { name: `${range} hue and saturation` }),
      ).toBeTruthy();
      expect(
        screen.getByRole("slider", { name: `${range} luminance` }),
      ).toBeTruthy();
    }
    expect(screen.queryByRole("slider", { name: /Global/ })).toBeNull();
    expect(screen.queryByText("Hue")).toBeNull();
    expect(screen.getByText("Blending")).toBeTruthy();
    expect(screen.getByText("Balance")).toBeTruthy();
  });

  it("switches between the 3-way view and one range without editing", () => {
    const change = renderGrading();
    fireEvent.click(screen.getByRole("button", { name: /Highlights/ }));
    expect(
      screen.getAllByRole("slider", { name: /hue and saturation/ }),
    ).toHaveLength(1);
    expect(screen.getByText("Hue")).toBeTruthy();
    expect(
      screen.queryByRole("slider", { name: "Shadows luminance" }),
    ).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "3-way" }));
    expect(
      screen.getAllByRole("slider", { name: /hue and saturation/ }),
    ).toHaveLength(3);
    expect(change).not.toHaveBeenCalled();
  });

  it("steps the luminance of the arc's own range with the keyboard", () => {
    const graded = {
      ...INITIAL_ADJUSTMENTS.colorGrading,
      shadows: { hue: 200, saturation: 30, luminance: 0 },
    };
    const change = renderGrading(graded);
    const arc = screen.getByRole("slider", { name: "Highlights luminance" });
    fireEvent.keyDown(arc, { key: "ArrowUp" });
    expect(lastGrading(change).highlights.luminance).toBe(1);
    expect(lastGrading(change).shadows).toEqual(graded.shadows);
    expect(lastGrading(change).midtones).toEqual(graded.midtones);
    fireEvent.keyDown(arc, { key: "ArrowDown", shiftKey: true });
    expect(lastGrading(change).highlights.luminance).toBe(-9);
    fireEvent.keyDown(arc, { key: "End" });
    expect(lastGrading(change).highlights.luminance).toBe(100);
    expect(arc.getAttribute("aria-valuenow")).toBe("100");
  });

  it("drags an arc as one interaction and shows the value meanwhile", () => {
    const interaction = vi.fn();
    const graded = {
      ...INITIAL_ADJUSTMENTS.colorGrading,
      shadows: { hue: 200, saturation: 30, luminance: 0 },
    };
    const change = renderGrading(graded, interaction);
    const arc = screen.getByRole("slider", { name: "Shadows luminance" });
    const svg = arc.closest("svg")!;
    // The cell's view box is 96 × 84 units from (8, 8); one pixel per unit.
    vi.spyOn(svg, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      width: 96,
      height: 84,
    } as DOMRect);
    (arc as unknown as Element).setPointerCapture = vi.fn();
    const at = (degrees: number) => {
      const a = (degrees * Math.PI) / 180;
      return {
        clientX: 50 + Math.cos(a) * 47 - 8,
        clientY: 50 - Math.sin(a) * 47 - 8,
      };
    };
    // Half-way up the arc is +50 (the arc spans ±50°).
    fireEvent.pointerDown(arc, { button: 0, ...at(25) });
    expect(interaction).toHaveBeenLastCalledWith(true);
    expect(screen.getByRole("status").textContent).toBe("+50");
    fireEvent.pointerMove(arc, at(-10));
    expect(screen.getByRole("status").textContent).toBe("−20");
    fireEvent.pointerUp(arc);
    expect(interaction).toHaveBeenLastCalledWith(false);
    expect(screen.queryByRole("status")).toBeNull();
    expect(lastGrading(change).shadows).toEqual({
      hue: 200,
      saturation: 30,
      luminance: -20,
    });
    expect(lastGrading(change).highlights).toEqual(graded.highlights);
  });

  it("resets an arc's luminance on double-click", () => {
    const change = renderGrading({
      ...INITIAL_ADJUSTMENTS.colorGrading,
      midtones: { hue: 40, saturation: 20, luminance: 35 },
    });
    const arc = screen.getByRole("slider", { name: "Midtones luminance" });
    expect(arc.getAttribute("aria-valuetext")).toBe("+35");
    fireEvent.doubleClick(arc);
    expect(lastGrading(change).midtones).toEqual({
      hue: 40,
      saturation: 20,
      luminance: 0,
    });
  });

  it("moves the wheel of its own range only", async () => {
    const change = renderGrading();
    fireEvent.keyDown(
      screen.getByRole("slider", { name: "Shadows hue and saturation" }),
      { key: "ArrowUp" },
    );
    await waitFor(() => expect(change).toHaveBeenCalled());
    expect(lastGrading(change).shadows.saturation).toBe(1);
    expect(lastGrading(change).midtones).toEqual(
      INITIAL_ADJUSTMENTS.colorGrading.midtones,
    );
  });
});

describe("black & white Auto", () => {
  const photo = {
    path: "/s/a.NEF",
    isReady: true,
    isRaw: true,
    width: 10,
    height: 10,
    exif: {},
  };

  it("asks for the photo's Auto mix and converts with it", async () => {
    useEditorStore.setState({
      selectedImage: photo as never,
      adjustments: { ...INITIAL_ADJUSTMENTS },
    });
    invoke.mockResolvedValue([-15, -26, -30, -31, -12, 27, 28, 6]);
    const change = vi.fn();
    render(
      <StudioBlackAndWhite
        values={{
          colorGrading: INITIAL_ADJUSTMENTS.colorGrading,
          sectionVisibility: INITIAL_ADJUSTMENTS.sectionVisibility,
        }}
        disabled={false}
        change={change}
        onInteraction={() => {}}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Auto black & white mix" }),
    );
    await waitFor(() => expect(change).toHaveBeenCalled());
    expect(invoke).toHaveBeenCalledWith("auto_gray_mix", {
      path: "/s/a.NEF",
      jsAdjustments: useEditorStore.getState().adjustments,
    });
    expect(change).toHaveBeenCalledWith({
      blackAndWhite: true,
      grayMixer: {
        red: -15,
        orange: -26,
        yellow: -30,
        green: -31,
        aqua: -12,
        blue: 27,
        purple: 28,
        magenta: 6,
      },
    });
  });

  it("ignores an answer for a photo that is no longer open", async () => {
    useEditorStore.setState({
      selectedImage: photo as never,
      adjustments: { ...INITIAL_ADJUSTMENTS },
    });
    let finish!: (value: number[]) => void;
    invoke.mockImplementation(
      () => new Promise((resolve) => (finish = resolve)),
    );
    const change = vi.fn();
    render(
      <StudioBlackAndWhite
        values={{
          colorGrading: INITIAL_ADJUSTMENTS.colorGrading,
          sectionVisibility: INITIAL_ADJUSTMENTS.sectionVisibility,
        }}
        disabled={false}
        change={change}
        onInteraction={() => {}}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Auto black & white mix" }),
    );
    useEditorStore.setState({
      selectedImage: { ...photo, path: "/s/b.NEF" } as never,
    });
    finish([0, 0, 0, 0, 0, 0, 0, 0]);
    await waitFor(() =>
      expect(
        (
          screen.getByRole("button", {
            name: "Auto black & white mix",
          }) as HTMLButtonElement
        ).disabled,
      ).toBe(false),
    );
    expect(change).not.toHaveBeenCalled();
  });
});
