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

const renderGrading = () => {
  const change = vi.fn();
  render(
    <StudioColorGrading
      values={{
        colorGrading: INITIAL_ADJUSTMENTS.colorGrading,
        sectionVisibility: INITIAL_ADJUSTMENTS.sectionVisibility,
      }}
      disabled={false}
      change={change}
      onInteraction={() => {}}
    />,
  );
  const header = screen.queryByRole("button", { name: /Color grading/ });
  if (header && !screen.queryByText("Blending")) fireEvent.click(header);
  return change;
};

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
