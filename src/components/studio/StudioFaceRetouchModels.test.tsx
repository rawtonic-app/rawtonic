import "../../i18n";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import StudioFaceRetouch, { faceRetouchGeometry, faceRetouchIsCurrent } from "./StudioFaceRetouch";
import { useEditorStore } from "../../store/useEditorStore";
import { INITIAL_ADJUSTMENTS, type FaceRetouch } from "../../utils/adjustments";
import { GEOMETRY_KEYS, ORIENTATION_KEYS } from "../../utils/geometryKeys";

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

describe("face retouch models", () => {
  it("downloads the face models only when asked (PRIVACY.md)", async () => {
    invoke.mockImplementation((_command: string, args: { download?: boolean }) =>
      args.download ? Promise.resolve({ frame: "f", faces: [] }) : Promise.reject("face-models-missing"),
    );
    useEditorStore.setState({
      selectedImage: { path: "/s/Capture/a.CR3", isReady: true } as never,
      adjustments: { ...INITIAL_ADJUSTMENTS, faceRetouch: undefined },
    });
    render(<StudioFaceRetouch onInteraction={() => {}} />);

    const button = await screen.findByRole("button", { name: "Download face models" });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][1]).toMatchObject({ download: false });

    fireEvent.click(button);
    await waitFor(() => expect(screen.getByText("No faces found in this photo.")).toBeTruthy());
    expect(invoke.mock.calls.at(-1)?.[1]).toMatchObject({ download: true });
  });
});

describe("face retouch frames", () => {
  const legacyRecipe = () => {
    const adjustments = { ...INITIAL_ADJUSTMENTS, fringeVersion: 1, lensVignetteAmount: 30 };
    // Faces found before frame version 2: the panel kept every geometry key.
    const geometry = JSON.stringify(faceRetouchGeometry(adjustments, [...ORIENTATION_KEYS, ...GEOMETRY_KEYS]));
    const faceRetouch = {
      settings: { blemishes: 40 },
      faces: [{ id: "f1", kept: [3], added: [7], settings: {}, blemishes: [], box: [0, 0, 10, 10] }],
      frame: '{"rotation":0}',
      geometry,
    } as unknown as FaceRetouch;
    return { ...adjustments, faceRetouch };
  };

  it("keeps an unchanged older retouch as it is, without finding faces again", async () => {
    invoke.mockResolvedValue({ frame: "2:{}", faces: [] });
    const adjustments = legacyRecipe();
    useEditorStore.setState({ selectedImage: { path: "/s/a.CR3", isReady: true } as never, adjustments });
    render(<StudioFaceRetouch onInteraction={() => {}} />);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(invoke).not.toHaveBeenCalled();
    expect(useEditorStore.getState().adjustments.faceRetouch).toBe(adjustments.faceRetouch);
    expect(faceRetouchIsCurrent(adjustments.faceRetouch, adjustments)).toBe(true);
    // Its rule: a vignetting change makes it stale, as in the engine.
    expect(faceRetouchIsCurrent(adjustments.faceRetouch, { ...adjustments, lensVignetteAmount: 50 })).toBe(false);
  });

  it("keeps a newer retouch when only colour corrections change", () => {
    const adjustments = { ...INITIAL_ADJUSTMENTS, fringeVersion: 1 };
    const faceRetouch = {
      settings: {},
      faces: [],
      frame: "2:{}",
      geometry: JSON.stringify(faceRetouchGeometry(adjustments)),
    } as unknown as FaceRetouch;
    expect(faceRetouchIsCurrent(faceRetouch, { ...adjustments, fringeVersion: 2, lensVignetteAmount: 50 })).toBe(true);
    expect(faceRetouchIsCurrent(faceRetouch, { ...adjustments, rotation: 3 })).toBe(false);
  });
});
