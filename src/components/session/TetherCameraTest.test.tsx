import "../../i18n";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import TetherCameraTest, { type TestedCamera } from "./TetherCameraTest";

type Callback = (event: { payload: unknown }) => void;
const listeners = vi.hoisted(() => new Map<string, Set<Callback>>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, callback: Callback) => {
    const all = listeners.get(name) ?? new Set<Callback>();
    listeners.set(name, all.add(callback));
    return Promise.resolve(() => all.delete(callback));
  }),
}));
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: () => Promise.resolve("9.9.9") }));
vi.mock("@tauri-apps/plugin-os", () => ({ platform: () => "macos", version: () => "26.0", arch: () => "aarch64" }));
const openUrl = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/plugin-shell", () => ({ open: openUrl }));

const emit = (name: string, payload: unknown) =>
  act(() => [...(listeners.get(name) ?? [])].forEach((callback) => callback({ payload })));

const CAMERA: TestedCamera = {
  model: "Fuji Fujifilm X-H2S",
  firmware: "4.10",
  serial: "93X1234567",
  transferQueue: true,
  library: "2.5.34",
  transport: "imageCaptureCore",
};
const SETTINGS = [{ key: "iso", label: "ISO", value: "160", choices: ["160", "200"], readOnly: false }];

let iso = "160";
let shot = 0;
/** What the mock camera does when fired: a photo in the library, its
 * Develop frame, and (on a camera with a transfer queue) its release. */
let onCapture: (n: number) => void;

beforeEach(() => {
  iso = "160";
  shot = 0;
  onCapture = (n) => {
    const destination = `/Users/someone/Jobs/Acme/Capture/DSCF${n}.RAF`;
    window.setTimeout(() => {
      emit("tether-captured", {
        cameraName: `DSCF${n}.RAF`,
        destination,
        assetId: `a${n}`,
        skipped: false,
        error: null,
        requested: true,
      });
      emit("tether-released", { cameraName: `DSCF${n}.RAF`, freed: true, error: null });
      emit("wgpu-frame-ready", { path: destination });
    }, 5);
  };
  invoke.mockImplementation((command: string, args?: { value?: string }) => {
    switch (command) {
      case "tether_settings":
        return Promise.resolve(SETTINGS.map((s) => ({ ...s, value: iso })));
      case "tether_set_setting":
        iso = args?.value ?? iso;
        return Promise.resolve(null);
      case "tether_capture":
        shot += 1;
        onCapture(shot);
        return Promise.resolve(null);
      default:
        return Promise.resolve(null);
    }
  });
});
afterEach(() => {
  cleanup();
  listeners.clear();
  invoke.mockReset();
});

const report = () => (screen.getByRole("textbox", { name: "Report (exactly what is copied)" }) as HTMLTextAreaElement).value;
const runStep = async (expected: RegExp) => {
  fireEvent.click(screen.getByRole("button", { name: "Run" }));
  await waitFor(() => expect(report()).toMatch(expected), { timeout: 4000 }).catch((e) => {
    console.log(report());
    throw e;
  });
};

it("runs the steps with the camera and reports them without serial number, paths or file names", async () => {
  render(<TetherCameraTest camera={CAMERA} connectMs={2310} freeCameraCopies onClose={vi.fn()} />);
  await waitFor(() => expect(report()).toContain("- Rawtonic: 9.9.9"));
  expect(report()).toContain("| Connect | pass | 2310 ms |");
  await runStep(/\| Read settings \| pass \| 1 read, 1 changeable; \d+ ms \|/);
  await runStep(/\| Change a setting \| pass \| ISO changed, read back and set back/);
  expect(iso).toBe("160");
  fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  expect(report()).toContain("| Autofocus | skipped | skipped by the user |");
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| pass \| 5\/5 in the library; photo on screen median \d+ ms/);
  await runStep(/\| Burst from Rawtonic \| pass \| 5\/5 in the library/);
  expect(shot).toBe(10);
  // A shot on the body.
  fireEvent.click(screen.getByRole("button", { name: "Run" }));
  await screen.findByText("Press the shutter button on the camera now.");
  emit("tether-captured", {
    cameraName: "DSCF99.RAF",
    destination: "/x/DSCF99.RAF",
    assetId: "b",
    skipped: false,
    error: null,
    requested: false,
  });
  emit("tether-released", { cameraName: "DSCF99.RAF", freed: true, error: null });
  await waitFor(() => expect(report()).toContain("| Shot on the camera | pass | 1/1 in the library |"));
  // The cable out and back in, then a shot.
  fireEvent.click(screen.getByRole("button", { name: "Run" }));
  await screen.findByText("Pull the USB cable out now.");
  emit("tether-state", { connected: false, error: { kind: "disconnected", message: "gone" } });
  await screen.findByText(/Plug the cable back in/);
  emit("tether-state", { connected: true });
  await waitFor(() => expect(report()).toMatch(/\| Cable out and back in \| pass \| 1\/1 in the library; \d+ ms;/));
  await runStep(/\| Transfer queue freed after commit \| pass \| 12 freed \|/);
  const text = report();
  expect(text).toContain("- Firmware: 4.10");
  expect(text).toContain("- Transfer queue: yes (freeing on)");
  expect(text).toContain("- System: macos 26.0 (aarch64)");
  expect(text).not.toMatch(/93X1234567|someone|Acme|DSCF|\/Users/);
  fireEvent.click(screen.getByRole("button", { name: "Open a camera report on GitHub" }));
  expect(openUrl).toHaveBeenCalledWith("https://github.com/rawtonic-app/rawtonic/issues/new?template=camera-report.yml");
});

it("fails a shot whose photo did not come in, with the reason scrubbed", async () => {
  onCapture = () =>
    window.setTimeout(
      () => emit("tether-capture-failed", { kind: "timeout", message: "DSCF7.RAF from 93X1234567 not at /Users/someone/x" }),
      5,
    );
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  expect(report()).toContain("| Connect | pass |  |");
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| FAIL \| 0\/5 in the library; <file> from <serial> not at <path> \|/);
});

it("finds a release before its photo's commit and skips the queue step on cameras without one", async () => {
  onCapture = (n) =>
    window.setTimeout(() => {
      emit("tether-released", { cameraName: `DSCF${n}.RAF`, freed: true, error: null });
      emit("tether-captured", {
        cameraName: `DSCF${n}.RAF`,
        destination: null,
        assetId: `a${n}`,
        skipped: false,
        error: null,
        requested: true,
      });
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| pass \| 5\/5/);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Transfer queue freed after commit \| FAIL \| 5 freed; 5 freed before their commit \|/);
  cleanup();
  render(
    <TetherCameraTest camera={{ ...CAMERA, transferQueue: false }} connectMs={null} freeCameraCopies onClose={vi.fn()} />,
  );
  for (const _ of [1, 2, 3, 4, 5, 6, 7]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Transfer queue freed after commit \| skipped \| the camera keeps no transfer queue \|/);
});
