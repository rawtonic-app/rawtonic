import "../../i18n";
import { useState } from "react";
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
import {
  StudioBlackAndWhite,
  StudioColorGrading,
  linkedGrades,
} from "./StudioColorTools";

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

describe("colour grading range switch", () => {
  it("is five icon-only segments named by tooltip and label", () => {
    renderGrading({
      ...INITIAL_ADJUSTMENTS.colorGrading,
      shadows: { hue: 200, saturation: 30, luminance: 0 },
    });
    const group = screen.getByRole("group", { name: "Tonal range" });
    const buttons = Array.from(group.querySelectorAll("button"));
    expect(buttons.map((b) => b.getAttribute("aria-label"))).toEqual([
      "3-way",
      "Shadows",
      "Midtones",
      "Highlights",
      "Global",
    ]);
    // No visible text that could be clipped in a narrow panel or a long
    // language; the selected range is named under its wheel instead.
    for (const b of buttons) expect(b.textContent).toBe("");
    // Only the graded range shows its tint.
    expect(group.querySelectorAll(".studio-grading-tint")).toHaveLength(1);
    expect(
      screen
        .getByRole("button", { name: "Shadows" })
        .querySelector(".studio-grading-tint"),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Global" }));
    expect(
      document.querySelector(".studio-wheel-row .studio-grading-3way-name")
        ?.textContent,
    ).toBe("Global");
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

describe("linked tonal wheels", () => {
  const grading = {
    ...INITIAL_ADJUSTMENTS.colorGrading,
    shadows: { hue: 350, saturation: 20, luminance: -10 },
    midtones: { hue: 110, saturation: 40, luminance: 5 },
    highlights: { hue: 230, saturation: 70, luminance: 15 },
  };

  it("wraps hue and preserves saturation offsets and individual luminance", () => {
    const result = linkedGrades(grading, "shadows", {
      ...grading.shadows,
      hue: 10,
      saturation: 30,
    });
    expect(result.shadows).toEqual({ hue: 10, saturation: 30, luminance: -10 });
    expect(result.midtones).toEqual({ hue: 130, saturation: 50, luminance: 5 });
    expect(result.highlights).toEqual({
      hue: 250,
      saturation: 80,
      luminance: 15,
    });
    expect(result.global).toEqual(grading.global);
    expect(grading.shadows.hue).toBe(350);
  });

  it("stops all wheels together at either saturation boundary", () => {
    const high = linkedGrades(grading, "shadows", {
      ...grading.shadows,
      saturation: 100,
    });
    expect([
      high.shadows.saturation,
      high.midtones.saturation,
      high.highlights.saturation,
    ]).toEqual([50, 70, 100]);
    const low = linkedGrades(grading, "highlights", {
      ...grading.highlights,
      saturation: 0,
    });
    expect([
      low.shadows.saturation,
      low.midtones.saturation,
      low.highlights.saturation,
    ]).toEqual([0, 20, 50]);
  });

  it("can tint neutral wheels without adding luminance", () => {
    const result = linkedGrades(INITIAL_ADJUSTMENTS.colorGrading, "midtones", {
      hue: 240,
      saturation: 20,
      luminance: 0,
    });
    for (const key of ["shadows", "midtones", "highlights"] as const)
      expect(result[key]).toEqual({ hue: 240, saturation: 20, luminance: 0 });
  });

  it("links keyboard wheel edits in one patch; toggling makes no recipe edit", async () => {
    const change = renderGrading(grading);
    const link = screen.getByRole("checkbox", { name: "Link tonal wheels" });
    fireEvent.click(link);
    expect(change).not.toHaveBeenCalled();
    fireEvent.keyDown(
      screen.getByRole("slider", { name: "Shadows hue and saturation" }),
      { key: "ArrowRight", shiftKey: true },
    );
    await waitFor(() => expect(change).toHaveBeenCalledTimes(1));
    expect(lastGrading(change).shadows.hue).toBe(0);
    expect(lastGrading(change).midtones.hue).toBe(120);
    expect(lastGrading(change).highlights.hue).toBe(240);
    fireEvent.keyDown(
      screen.getByRole("slider", { name: "Highlights luminance" }),
      { key: "ArrowUp" },
    );
    expect(lastGrading(change).shadows).toEqual(grading.shadows);
    expect(lastGrading(change).highlights.luminance).toBe(16);
    fireEvent.click(link);
    fireEvent.keyDown(
      screen.getByRole("slider", { name: "Midtones hue and saturation" }),
      { key: "ArrowUp" },
    );
    await waitFor(() =>
      expect(lastGrading(change).midtones.saturation).toBe(41),
    );
    expect(lastGrading(change).shadows).toEqual(grading.shadows);
  });
});

it("keeps a linked drag constrained on screen and in one interaction", async () => {
  const interaction = vi.fn();
  const change = vi.fn();
  function Controlled() {
    const [colorGrading, setGrading] = useState({
      ...INITIAL_ADJUSTMENTS.colorGrading,
      shadows: { hue: 0, saturation: 20, luminance: 0 },
      midtones: { hue: 120, saturation: 40, luminance: 0 },
      highlights: { hue: 240, saturation: 90, luminance: 0 },
    });
    return (
      <StudioColorGrading
        values={{
          colorGrading,
          sectionVisibility: INITIAL_ADJUSTMENTS.sectionVisibility,
        }}
        disabled={false}
        onInteraction={interaction}
        change={(patch) => {
          change(patch);
          setGrading(patch.colorGrading!);
        }}
      />
    );
  }
  render(<Controlled />);
  const header = screen.queryByRole("button", { name: /Color grading/ });
  if (header && !screen.queryByText("Blending")) fireEvent.click(header);
  fireEvent.click(screen.getByRole("checkbox", { name: "Link tonal wheels" }));
  const wheel = screen.getByRole("slider", {
    name: "Shadows hue and saturation",
  });
  vi.spyOn(wheel, "getBoundingClientRect").mockReturnValue({
    left: 0,
    top: 0,
    width: 100,
    height: 100,
  } as DOMRect);
  wheel.setPointerCapture = vi.fn();
  fireEvent.pointerDown(wheel, { button: 0, clientX: 100, clientY: 50 });
  await waitFor(() =>
    expect(lastGrading(change).highlights.saturation).toBe(100),
  );
  expect(wheel.getAttribute("aria-valuenow")).toBe("30");
  fireEvent.pointerMove(wheel, { clientX: 50, clientY: 0 });
  await waitFor(() => expect(lastGrading(change).shadows.hue).toBe(90));
  expect(lastGrading(change).midtones).toEqual({
    hue: 210,
    saturation: 50,
    luminance: 0,
  });
  fireEvent.pointerUp(wheel);
  expect(interaction.mock.calls).toEqual([[true], [false]]);
});

it("previews all linked indicators before the parent commits a drag", async () => {
  const change = renderGrading();
  fireEvent.click(screen.getByRole("checkbox", { name: "Link tonal wheels" }));
  const wheel = screen.getByRole("slider", {
    name: "Shadows hue and saturation",
  });
  vi.spyOn(wheel, "getBoundingClientRect").mockReturnValue({
    left: 0,
    top: 0,
    width: 100,
    height: 100,
  } as DOMRect);
  wheel.setPointerCapture = vi.fn();
  fireEvent.pointerDown(wheel, { button: 0, clientX: 75, clientY: 50 });
  await waitFor(() => expect(change).toHaveBeenCalled());
  await waitFor(() => {
    for (const name of ["Shadows", "Midtones", "Highlights"])
      expect(
        screen
          .getByRole("slider", { name: `${name} hue and saturation` })
          .getAttribute("aria-valuenow"),
      ).toBe("50");
  });
  fireEvent.pointerUp(wheel);
});
