import "../../i18n";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import TetherPanel, { type TetherStatus } from "./TetherPanel";
import { TETHER_CAPTURE_EVENT, isTetherConnected } from "./tether";

const callbacks = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, callback: (event: { payload: unknown }) => void) => {
    callbacks.set(name, callback);
    return Promise.resolve(() => callbacks.delete(name));
  }),
}));
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke, isTauri: () => true }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn(), warning: vi.fn(), success: vi.fn() } }));

const STATUS: TetherStatus = {
  session: "/jobs/Acme",
  camera: { model: "Sony ILCE-7M4", port: "usb:020,004", battery: "80%", captureTarget: null, canCapture: true, canPreview: true },
};
const SETTINGS = [
  { key: "iso", cameraKey: "iso", label: "ISO", value: "100", choices: ["100", "200"], readOnly: false },
  { key: "aperture", cameraKey: "f-number", label: "Aperture", value: "f/8", choices: ["f/8"], readOnly: true },
];

beforeEach(() => {
  invoke.mockImplementation((command: string) => {
    switch (command) {
      case "tether_cameras":
        return Promise.resolve({ library: "2.5.34", cameras: [{ model: "Sony ILCE-7M4", port: "usb:020,004" }] });
      case "tether_connect":
        return Promise.resolve(STATUS);
      case "tether_settings":
        return Promise.resolve(SETTINGS);
      case "load_presets":
        return Promise.resolve([]);
      default:
        return Promise.resolve(null);
    }
  });
});
afterEach(() => {
  cleanup();
  callbacks.clear();
  invoke.mockReset();
});

function Harness({ initial = null as TetherStatus | null, onStatus = vi.fn() }) {
  return <TetherPanel directory="/jobs/Acme" jobName="Acme" status={initial} onStatus={onStatus} onClose={vi.fn()} />;
}

it("connects the camera found on USB for this library, with the next-capture settings", async () => {
  const onStatus = vi.fn();
  render(<Harness onStatus={onStatus} />);
  await waitFor(() => expect(screen.getByRole("option", { name: "Sony ILCE-7M4" })).toBeTruthy());
  fireEvent.change(screen.getByLabelText("Next capture rating"), { target: { value: "3" } });
  fireEvent.change(screen.getByLabelText("Next capture keywords"), { target: { value: "studio, Acme" } });
  fireEvent.click(screen.getByRole("button", { name: "Connect" }));
  await waitFor(() => expect(onStatus).toHaveBeenCalledWith(STATUS));
  expect(invoke).toHaveBeenCalledWith("tether_connect", {
    directory: "/jobs/Acme",
    port: "usb:020,004",
    template: "{orig}",
    nextCapture: { mode: "defaults" },
    metadata: { rating: 3, keywords: ["studio", "Acme"] },
  });
});

it("fires from the button and the Capture shortcut, shows settings set on the body as such", async () => {
  render(<Harness initial={STATUS} />);
  await waitFor(() => expect(screen.getByLabelText("ISO")).toBeTruthy());
  expect(isTetherConnected()).toBe(true);
  // Aperture is set on the body: shown, not offered.
  expect(screen.queryByRole("combobox", { name: "Aperture" })).toBeNull();
  expect(screen.getByText("f/8").tagName).toBe("OUTPUT");
  fireEvent.change(screen.getByLabelText("ISO"), { target: { value: "200" } });
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("tether_set_setting", { key: "iso", value: "200" }));
  fireEvent.click(screen.getByRole("button", { name: "Capture" }));
  act(() => {
    window.dispatchEvent(new Event(TETHER_CAPTURE_EVENT));
  });
  await waitFor(() => expect(invoke.mock.calls.filter(([c]) => c === "tether_capture")).toHaveLength(2));
  act(() =>
    callbacks.get("tether-captured")!({
      payload: { cameraName: "DSC00001.ARW", destination: "/jobs/Acme/Capture/DSC00001.ARW", assetId: "a", skipped: false, error: null, requested: true, msToEvent: 120, msToDownloaded: 480, msToIngested: 530 },
    }),
  );
  expect(screen.getByText(/DSC00001.ARW in the library 530 ms/)).toBeTruthy();
});

it("an unplugged camera is reported and the panel offers to connect again", async () => {
  const onStatus = vi.fn();
  render(<Harness initial={STATUS} onStatus={onStatus} />);
  await waitFor(() => expect(callbacks.has("tether-state")).toBe(true));
  act(() =>
    callbacks.get("tether-state")!({
      payload: { connected: false, error: { kind: "disconnected", message: "The camera was disconnected." } },
    }),
  );
  expect(onStatus).toHaveBeenCalledWith(null);
});
