import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import StudioLensCorrection from "./StudioLensCorrection";
import { useEditorStore } from "../../store/useEditorStore";
import { CAMERA_CA_VERSION, INITIAL_ADJUSTMENTS, type Adjustments } from "../../utils/adjustments";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
let current: Adjustments;
const setAdjustments = vi.fn((update: Adjustments | ((prev: Adjustments) => Adjustments)) => {
  current = typeof update === "function" ? update(current) : update;
  useEditorStore.setState({ adjustments: current });
});
vi.mock("../../hooks/useEditorActions", () => ({
  useEditorActions: () => ({ setAdjustments }),
  isCurrentEditorImage: (path: string) => {
    const image = useEditorStore.getState().selectedImage;
    return image?.path === path && image.isReady;
  },
}));
const mockInvoke = vi.mocked(invoke);
const distortion = { k1: 0.01, k2: 0, k3: 0 };
const cameraParams = { k1: -0.02, k2: 0.05, k3: -0.04, model: 1 };

beforeEach(() => {
  current = { ...INITIAL_ADJUSTMENTS };
  setAdjustments.mockClear();
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command, args) => {
    const a = args as Record<string, unknown>;
    if (command === "get_lensfun_makers") return ["Sony", "Fujifilm"];
    if (command === "get_lensfun_lenses_for_maker") return a.maker === "Sony" ? ["FE 85mm F1.8"] : ["XF 23mm"];
    if (command === "autodetect_lens") return a.model === "FE 85mm F1.8" ? ["Sony", "FE 85mm F1.8"] : null;
    if (command === "get_lens_distortion_params") return distortion;
    if (command === "get_camera_lens_profile")
      return a.path === "/job/x.raf" ? { maker: "Fujifilm", params: cameraParams, vignetting: true } : null;
    return undefined;
  });
  useEditorStore.setState({
    adjustments: current,
    selectedImage: {
      path: "/job/a.arw",
      isReady: true,
      exif: { Make: "SONY", Model: "ILCE-7M3", LensModel: "FE 85mm F1.8", FocalLength: "85.0 mm", FNumber: "2.5" },
    } as never,
  });
});
afterEach(cleanup);

const renderTool = async () => {
  render(<StudioLensCorrection />);
  await act(async () => {});
};

describe("Lens correction tool", () => {
  it("enables the detected profile with the photo's focal length and aperture", async () => {
    await renderTool();
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Enable profile corrections"));
    });
    expect(mockInvoke).toHaveBeenCalledWith("autodetect_lens", {
      maker: "SONY",
      model: "FE 85mm F1.8",
      cameraModel: "ILCE-7M3",
    });
    expect(mockInvoke).toHaveBeenCalledWith("get_lens_distortion_params", {
      maker: "Sony",
      model: "FE 85mm F1.8",
      focalLength: 85,
      aperture: 2.5,
      distance: 0,
    });
    expect(current).toMatchObject({
      lensMaker: "Sony",
      lensModel: "FE 85mm F1.8",
      lensDistortionParams: distortion,
      lensCorrectionMode: "auto",
    });
    expect((screen.getByLabelText("Enable profile corrections") as HTMLInputElement).checked).toBe(true);
    // Turning it off removes the profile and its amounts.
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Enable profile corrections"));
    });
    expect(current).toMatchObject({ lensMaker: null, lensModel: null, lensDistortionParams: null });
  });

  it("says when no profile is found and lets the lens be chosen", async () => {
    useEditorStore.setState({
      selectedImage: { path: "/job/b.raf", isReady: true, exif: { Make: "FUJIFILM", Model: "X-H2S", LensModel: "Unknown" } } as never,
    });
    await renderTool();
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Enable profile corrections"));
    });
    expect(screen.getByText(/No profile found for Unknown/)).toBeTruthy();
    expect(current.lensMaker).toBeNull();
    await act(async () => {
      fireEvent.change(screen.getByLabelText("Lens make"), { target: { value: "Fujifilm" } });
    });
    await act(async () => {
      fireEvent.change(screen.getByLabelText("Lens model"), { target: { value: "XF 23mm" } });
    });
    expect(current).toMatchObject({ lensMaker: "Fujifilm", lensModel: "XF 23mm", lensCorrectionMode: "manual" });
  });

  it("disables the amounts without a profile", async () => {
    await renderTool();
    const sliders = screen.getAllByRole("slider") as HTMLInputElement[];
    expect(sliders.length).toBe(3);
    expect(sliders.every((slider) => slider.disabled)).toBe(true);
  });

  it("turns on the measured chromatic aberration removal, with or without a profile", async () => {
    await renderTool();
    fireEvent.click(screen.getByRole("checkbox", { name: "Remove chromatic aberration" }));
    expect(current.autoChromaticAberration).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: "Remove chromatic aberration" }));
    expect(current.autoChromaticAberration).toBeUndefined();
  });
  it("shows the automatic default of new RAW edits and stores only a change from it", async () => {
    current = { ...INITIAL_ADJUSTMENTS, autoCaVersion: 2 };
    useEditorStore.setState({ adjustments: current });
    await renderTool();
    const box = () => screen.getByRole("checkbox", { name: "Remove chromatic aberration" }) as HTMLInputElement;
    expect(box().checked).toBe(true);
    fireEvent.click(box());
    expect(current.autoChromaticAberration).toBe(false);
    fireEvent.click(box());
    expect(current.autoChromaticAberration).toBeUndefined();
  });
  it("leaves the camera's own CA data in place by default in version 2 recipes", async () => {
    current = {
      ...INITIAL_ADJUSTMENTS,
      autoCaVersion: 2,
      lensCorrectionMode: "camera",
      lensDistortionParams: { ...cameraParams, tca_vr: 1.0012, tca_vb: 0.9991 },
    } as Adjustments;
    useEditorStore.setState({ adjustments: current });
    await renderTool();
    const box = screen.getByRole("checkbox", { name: "Remove chromatic aberration" }) as HTMLInputElement;
    expect(box.checked).toBe(false);
    fireEvent.click(box);
    expect(current.autoChromaticAberration).toBe(true);
  });
  it("lets new edits measure against the camera's CA data, and off keeps the camera's", async () => {
    current = {
      ...INITIAL_ADJUSTMENTS,
      autoCaVersion: 3,
      lensCorrectionMode: "camera",
      lensDistortionParams: { ...cameraParams, tca_vr: 1.0012, tca_vb: 0.9991 },
    } as Adjustments;
    useEditorStore.setState({ adjustments: current });
    await renderTool();
    const box = () => screen.getByRole("checkbox", { name: "Remove chromatic aberration" }) as HTMLInputElement;
    expect(box().checked).toBe(true);
    fireEvent.click(box());
    expect(current.autoChromaticAberration).toBe(false);
    fireEvent.click(box());
    expect(current.autoChromaticAberration).toBeUndefined();
  });
  it("drops a detected lens that arrives after another photograph opened", async () => {
    await renderTool();
    let finish!: (value: unknown) => void;
    mockInvoke.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    fireEvent.click(screen.getByLabelText("Enable profile corrections"));
    const next = { ...INITIAL_ADJUSTMENTS };
    act(() => useEditorStore.setState({
      selectedImage: { path: "/job/b.arw", isReady: true, exif: {} } as never,
      adjustments: next,
    }));
    await act(async () => finish(["Sony", "FE 85mm F1.8"]));
    expect(setAdjustments).not.toHaveBeenCalled();
    expect(useEditorStore.getState().adjustments).toBe(next);
  });
});

describe("camera lens data", () => {
  beforeEach(() => {
    useEditorStore.setState({
      selectedImage: {
        path: "/job/x.raf",
        isReady: true,
        exif: { Make: "FUJIFILM", Model: "X-H2S", LensModel: "XF8-16mmF2.8 R LM WR" },
      } as never,
    });
  });

  it("is preferred over the lens database and can be swapped for it", async () => {
    await renderTool();
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Enable profile corrections"));
    });
    expect(current).toMatchObject({
      lensCorrectionMode: "camera",
      lensDistortionParams: cameraParams,
      lensMaker: null,
      cameraCaVersion: CAMERA_CA_VERSION,
    });
    expect(mockInvoke).not.toHaveBeenCalledWith("autodetect_lens", expect.anything());
    expect(screen.getByText(/Camera data \(Fujifilm\)/)).toBeTruthy();
    // Without a TCA curve in the camera data, its amount is not offered.
    const sliders = screen.getAllByRole("slider") as HTMLInputElement[];
    expect(sliders.map((s) => s.disabled)).toEqual([false, false, true]);
    await act(async () => {
      fireEvent.click(screen.getByText("Use lens database…"));
    });
    expect(current).toMatchObject({ lensCorrectionMode: "manual", lensDistortionParams: null });
    expect(screen.getByLabelText("Lens make")).toBeTruthy();
  });
});
