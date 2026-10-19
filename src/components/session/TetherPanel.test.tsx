import "../../i18n";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import TetherPanel, { ramOnly, type TetherStatus } from "./TetherPanel";
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

function Harness({ initial = null as TetherStatus | null, onStatus = vi.fn(), hidden = false }) {
  return <TetherPanel hidden={hidden} directory="/jobs/Acme" jobName="Acme" status={initial} onStatus={onStatus} onClose={vi.fn()} />;
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
    freeCameraCopies: true,
  });
});

it("frees photos on a camera with a transfer queue unless the user turns it off", async () => {
  localStorage.removeItem("rawtonic.tether.freeCameraCopies");
  render(<Harness initial={STATUS} />);
  await waitFor(() => expect(screen.getByLabelText("ISO")).toBeTruthy());
  expect(screen.queryByRole("checkbox", { name: /Free photos on the camera/ })).toBeNull();
  cleanup();
  render(<Harness initial={{ ...STATUS, camera: { ...STATUS.camera, transferQueue: true } }} />);
  const box = await screen.findByRole("checkbox", { name: /Free photos on the camera/ });
  expect((box as HTMLInputElement).checked).toBe(true);
  fireEvent.click(box);
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("tether_next_capture", expect.objectContaining({ freeCameraCopies: false })),
  );
  expect(localStorage.getItem("rawtonic.tether.freeCameraCopies")).toBe("0");
  localStorage.removeItem("rawtonic.tether.freeCameraCopies");
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

it("a closed panel of a connected camera still fires on the Capture shortcut", async () => {
  render(<Harness initial={STATUS} hidden />);
  await waitFor(() => expect(isTetherConnected()).toBe(true));
  expect(screen.queryByRole("button", { name: "Capture" })).toBeNull();
  act(() => {
    window.dispatchEvent(new Event(TETHER_CAPTURE_EVENT));
  });
  await waitFor(() => expect(invoke.mock.calls.filter(([c]) => c === "tether_capture")).toHaveLength(1));
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

it("a camera plugged in while the panel is open shows up without a refresh", async () => {
  let cameras: { model: string; port: string }[] = [];
  invoke.mockImplementation((command: string) =>
    Promise.resolve(command === "tether_cameras" ? { library: "2.5.34", cameras } : command === "load_presets" ? [] : null),
  );
  vi.useFakeTimers({ shouldAdvanceTime: true });
  try {
    render(<Harness />);
    await waitFor(() => expect(screen.getByText("No camera found on USB")).toBeTruthy());
    cameras = [{ model: "Sony ILCE-7M4", port: "usb:020,004" }];
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2500);
    });
    await waitFor(() => expect(screen.getByRole("option", { name: "Sony ILCE-7M4" })).toBeTruthy());
  } finally {
    vi.useRealTimers();
  }
});

it("a camera that came back is shown as connected again, without an error", async () => {
  let setStatus: (status: TetherStatus | null) => void = () => undefined;
  function Stateful() {
    const [status, set] = useState<TetherStatus | null>(STATUS);
    setStatus = set;
    return <TetherPanel directory="/jobs/Acme" jobName="Acme" status={status} onStatus={set} onClose={vi.fn()} />;
  }
  render(<Stateful />);
  await waitFor(() => expect(callbacks.has("tether-state")).toBe(true));
  act(() =>
    callbacks.get("tether-state")!({
      payload: { connected: false, error: { kind: "disconnected", message: "The camera was disconnected." } },
    }),
  );
  expect(screen.getByText(/connects the camera again when it is back/)).toBeTruthy();
  // The workspace sets the status again; the panel clears its error.
  act(() => {
    setStatus(STATUS);
    callbacks.get("tether-state")!({ payload: { connected: true, status: STATUS } });
  });
  await waitFor(() => expect(screen.queryByText("The camera was disconnected.")).toBeNull());
  expect(screen.queryByText(/connects the camera again when it is back/)).toBeNull();
});

it("says when a camera keeps its photos only in its memory", () => {
  expect(ramOnly([{ key: "capturetarget", value: "Internal RAM" }])).toBe(true);
  expect(ramOnly([{ key: "capturetarget", value: "sdram" }])).toBe(true);
  expect(ramOnly([{ key: "capturetarget", value: "card+sdram" }])).toBe(false);
  expect(ramOnly([{ key: "capturetarget", value: "Memory card" }])).toBe(false);
  expect(ramOnly([{ key: "iso", value: "100" }])).toBe(false);
});

it("fires on the Capture shortcut with no panel mounted (fullscreen) and keeps the next-capture choice", async () => {
  const { unmount } = render(<Harness initial={STATUS} />);
  await waitFor(() => expect(screen.getByLabelText("ISO")).toBeTruthy());
  fireEvent.click(screen.getByText("Next capture"));
  fireEvent.change(screen.getByLabelText("Next capture rating"), { target: { value: "4" } });
  unmount();
  expect(isTetherConnected()).toBe(true);
  act(() => {
    window.dispatchEvent(new Event(TETHER_CAPTURE_EVENT));
  });
  await waitFor(() => expect(invoke.mock.calls.filter(([c]) => c === "tether_capture")).toHaveLength(1));
  // Shown again: the choice the connection uses, not the defaults.
  render(<Harness initial={STATUS} />);
  await waitFor(() => expect(screen.getByLabelText("ISO")).toBeTruthy());
  fireEvent.click(screen.getByText("Next capture"));
  expect((screen.getByLabelText("Next capture rating") as HTMLSelectElement).value).toBe("4");
});

it("stops the camera's live view when the panel goes away", async () => {
  const { unmount } = render(<Harness initial={STATUS} />);
  await waitFor(() => expect(screen.getByLabelText("ISO")).toBeTruthy());
  fireEvent.click(screen.getByRole("button", { name: "Live view" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("tether_live_view", { on: true }));
  unmount();
  expect(invoke).toHaveBeenCalledWith("tether_live_view", { on: false });
});

it("stops a live view whose start was still on its way when the panel went away", async () => {
  let answer: (value: null) => void = () => {};
  const base = invoke.getMockImplementation()!;
  invoke.mockImplementation((command: string, args?: unknown) =>
    command === "tether_live_view" && (args as { on: boolean }).on
      ? new Promise((resolve) => (answer = resolve))
      : base(command, args),
  );
  const { unmount } = render(<Harness initial={STATUS} />);
  await waitFor(() => expect(screen.getByLabelText("ISO")).toBeTruthy());
  fireEvent.click(screen.getByRole("button", { name: "Live view" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("tether_live_view", { on: true }));
  unmount();
  expect(invoke).toHaveBeenCalledWith("tether_live_view", { on: false });
  answer(null);
});

it("says when a camera model has not been tested yet", async () => {
  render(<Harness initial={STATUS} />);
  await waitFor(() => expect(screen.getByLabelText("ISO")).toBeTruthy());
  expect(screen.getByText(/not yet tested with this camera model/)).toBeTruthy();
  cleanup();
  render(<Harness initial={{ ...STATUS, camera: { ...STATUS.camera, model: "Fuji Fujifilm X-H2S" } }} />);
  await waitFor(() => expect(screen.getByLabelText("ISO")).toBeTruthy());
  expect(screen.queryByText(/not yet tested with this camera model/)).toBeNull();
});
