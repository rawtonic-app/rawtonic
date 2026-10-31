import "../../i18n";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import StudioWhiteBalance from "./StudioWhiteBalance";
import { KELVIN_SCALE } from "../../utils/whiteBalance";

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

const flushFrames = () =>
  act(() => new Promise((resolve) => requestAnimationFrame(() => resolve(0))));

describe("white balance", () => {
  it("shows the camera's As Shot Kelvin and tint", async () => {
    invoke.mockResolvedValue({ temperature: 6150, tint: 7 });
    render(
      <StudioWhiteBalance
        values={{}}
        imagePath="/s/a.NEF"
        disabled={false}
        onAuto={vi.fn()}
        onChange={vi.fn()}
      />,
    );
    const temp = await screen.findByLabelText(
      "White balance temperature value (K)",
    );
    expect(invoke).toHaveBeenCalledWith("white_balance_info", {
      imagePath: "/s/a.NEF",
    });
    expect((temp as HTMLInputElement).value).toBe("6150");
    expect(
      (screen.getByLabelText("White balance tint value") as HTMLInputElement)
        .value,
    ).toBe("7");
    expect(
      (screen.getByLabelText("White balance mode") as HTMLSelectElement).value,
    ).toBe("asShot");
  });

  it("puts the picker in the WB row beside the presets, as both reference editors do", async () => {
    invoke.mockResolvedValue({ temperature: 6150, tint: 7 });
    render(
      <StudioWhiteBalance
        values={{}}
        imagePath="/s/a.NEF"
        disabled={false}
        onAuto={vi.fn()}
        onChange={vi.fn()}
        picker={<button type="button" aria-label="White balance picker" />}
        fallback={<span>relative</span>}
      />,
    );
    const mode = await screen.findByLabelText("White balance mode");
    const picker = screen.getByRole("button", { name: "White balance picker" });
    // One row: the picker is no separate row above the presets.
    expect(mode.closest(".studio-inline")).toBe(picker.closest(".studio-inline"));
    // A press on the picker does not open the presets (not inside their label).
    expect(picker.closest("label")).toBeNull();
  });

  it("switches to presets and to Custom when a slider moves", async () => {
    invoke.mockResolvedValue({ temperature: 6150, tint: 7 });
    const onChange = vi.fn();
    const { rerender } = render(
      <StudioWhiteBalance
        values={{}}
        imagePath="/s/a.NEF"
        disabled={false}
        onAuto={vi.fn()}
        onChange={onChange}
      />,
    );
    const mode = await screen.findByLabelText("White balance mode");
    fireEvent.change(mode, { target: { value: "tungsten" } });
    expect(onChange).toHaveBeenLastCalledWith({
      whiteBalance: "tungsten",
      wbTemperature: undefined,
      wbTint: undefined,
    });
    rerender(
      <StudioWhiteBalance
        values={{ whiteBalance: "tungsten" }}
        imagePath="/s/a.NEF"
        disabled={false}
        onAuto={vi.fn()}
        onChange={onChange}
      />,
    );
    expect(
      (
        screen.getByLabelText(
          "White balance temperature value (K)",
        ) as HTMLInputElement
      ).value,
    ).toBe("2850");
    // The track runs in mireds: its position for 4000 K.
    fireEvent.change(screen.getByLabelText("White balance temperature"), {
      target: { value: String(KELVIN_SCALE.toTrack(4000)) },
    });
    await flushFrames();
    expect(onChange).toHaveBeenLastCalledWith({
      whiteBalance: "custom",
      wbTemperature: 4000,
      wbTint: 0,
    });
    fireEvent.change(mode, { target: { value: "asShot" } });
    expect(onChange).toHaveBeenLastCalledWith({
      whiteBalance: undefined,
      wbTemperature: undefined,
      wbTint: undefined,
    });
  });

  it("Auto in the menu asks for the photo's Auto balance", async () => {
    invoke.mockResolvedValue({ temperature: 6150, tint: 7 });
    const onAuto = vi.fn();
    const onChange = vi.fn();
    render(
      <StudioWhiteBalance
        values={{}}
        imagePath="/s/a.NEF"
        disabled={false}
        onAuto={onAuto}
        onChange={onChange}
      />,
    );
    const mode = await screen.findByLabelText("White balance mode");
    fireEvent.change(mode, { target: { value: "auto" } });
    expect(onAuto).toHaveBeenCalledTimes(1);
    expect(onChange).not.toHaveBeenCalled();
  });

  it("falls back to the relative sliders without a camera calibration", async () => {
    invoke.mockResolvedValue(null);
    render(
      <StudioWhiteBalance
        values={{}}
        imagePath="/s/a.jpg"
        disabled={false}
        onAuto={vi.fn()}
        onChange={vi.fn()}
        fallback={<span>relative sliders</span>}
      />,
    );
    await screen.findByText("relative sliders");
    expect(screen.queryByLabelText("White balance mode")).toBeNull();
  });

  it("maps Kelvin to mireds and back", () => {
    for (const k of [2000, 2850, 5500, 12000, 50000])
      expect(KELVIN_SCALE.fromTrack(KELVIN_SCALE.toTrack(k))).toBe(k);
    // Warm light gets more of the track than blue sky.
    const span = (a: number, b: number) =>
      KELVIN_SCALE.toTrack(b) - KELVIN_SCALE.toTrack(a);
    expect(span(2000, 5500)).toBeGreaterThan(span(5500, 50000));
  });
});
