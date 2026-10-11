import "../../i18n";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import StudioFaceRetouch from "./StudioFaceRetouch";
import { useEditorStore } from "../../store/useEditorStore";
import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";

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
    await waitFor(() => expect(screen.getByText("No faces found in this photograph.")).toBeTruthy());
    expect(invoke.mock.calls.at(-1)?.[1]).toMatchObject({ download: true });
  });
});
