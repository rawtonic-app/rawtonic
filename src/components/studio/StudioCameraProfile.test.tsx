import "../../i18n";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const { invoke, open } = vi.hoisted(() => ({ invoke: vi.fn(), open: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open }));

import StudioCameraProfile from "./StudioCameraProfile";

afterEach(() => {
  cleanup();
  invoke.mockReset();
  open.mockReset();
});

const studio = {
  value: "profiles/Studio.dcp",
  name: "Studio flash",
  camera: "Canon EOS R5",
};

describe("camera profile choice", () => {
  it("lists the session's profiles and chooses one", async () => {
    invoke.mockResolvedValue([studio]);
    const onChange = vi.fn();
    render(
      <StudioCameraProfile
        value={undefined}
        imagePath="/s/Capture/a.CR3"
        disabled={false}
        onChange={onChange}
      />,
    );
    await screen.findByRole("option", { name: "Studio flash" });
    expect(invoke).toHaveBeenCalledWith("list_camera_profiles", {
      imagePath: "/s/Capture/a.CR3",
    });
    fireEvent.change(screen.getByLabelText("Camera profile"), {
      target: { value: studio.value },
    });
    expect(onChange).toHaveBeenCalledWith(studio.value);
    fireEvent.change(screen.getByLabelText("Camera profile"), {
      target: { value: "" },
    });
    expect(onChange).toHaveBeenLastCalledWith(undefined);
  });

  it("imports a DCP into the session and selects it", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "list_camera_profiles" ? [] : studio),
    );
    open.mockResolvedValue("/Downloads/Studio.dcp");
    const onChange = vi.fn();
    render(
      <StudioCameraProfile
        value={undefined}
        imagePath="/s/Capture/a.CR3"
        disabled={false}
        onChange={onChange}
      />,
    );
    fireEvent.change(screen.getByLabelText("Camera profile"), {
      target: { value: "__import__" },
    });
    await waitFor(() => expect(onChange).toHaveBeenCalledWith(studio.value));
    expect(invoke).toHaveBeenCalledWith("import_camera_profile", {
      source: "/Downloads/Studio.dcp",
      imagePath: "/s/Capture/a.CR3",
    });
  });

  it("shows why an import failed", async () => {
    invoke.mockImplementation((command: string) =>
      command === "list_camera_profiles"
        ? Promise.resolve([])
        : Promise.reject("Not a DCP camera profile"),
    );
    open.mockResolvedValue("/Downloads/x.dcp");
    render(
      <StudioCameraProfile
        value={undefined}
        imagePath="/s/Capture/a.CR3"
        disabled={false}
        onChange={() => {}}
      />,
    );
    fireEvent.change(screen.getByLabelText("Camera profile"), {
      target: { value: "__import__" },
    });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("The operation could not be completed.");
    expect(alert.querySelector("small")?.textContent).toBe("Not a DCP camera profile");
  });
});

it("reports native profile picker failures with translated context", async () => {
  invoke.mockResolvedValue([]);
  open.mockRejectedValue(new Error("Picker unavailable"));
  render(<StudioCameraProfile value={undefined} imagePath="/s/Capture/a.CR3" disabled={false} onChange={vi.fn()} />);
  fireEvent.change(screen.getByLabelText("Camera profile"), { target: { value: "__import__" } });
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toContain("The operation could not be completed.");
  expect(alert.querySelector("small")?.textContent).toBe("Error: Picker unavailable");
});
