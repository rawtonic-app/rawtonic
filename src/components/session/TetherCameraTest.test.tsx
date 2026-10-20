import "../../i18n";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
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
  await waitFor(() => expect(report()).toContain("| Shot on the camera | pass | 1/1 in the library |"), { timeout: 4000 });
  // The cable out and back in, then a shot.
  fireEvent.click(screen.getByRole("button", { name: "Run" }));
  await screen.findByText("Pull the USB cable out now.");
  emit("tether-state", { connected: false, error: { kind: "disconnected", message: "gone" } });
  await screen.findByText(/Plug the cable back in/);
  emit("tether-state", { connected: true });
  await waitFor(() => expect(report()).toMatch(/\| Cable out and back in \| pass \| 1\/1 in the library; \d+ ms;/), {
    timeout: 4000,
  });
  await runStep(/\| Transfer queue freed after commit \| pass \| 12 freed \|/);
  const text = report();
  expect(text).toContain("- Firmware: 4.10");
  expect(text).toContain("- Transfer queue: yes (freeing on)");
  expect(text).toContain("- System: macos 26.0 (aarch64)");
  expect(text).not.toMatch(/93X1234567|someone|Acme|DSCF|\/Users/);
  fireEvent.click(screen.getByRole("button", { name: "Open a camera report on GitHub" }));
  expect(openUrl).toHaveBeenCalledWith("https://github.com/rawtonic-app/rawtonic/issues/new?template=camera-report.yml");
}, 20_000);

it("fails a shot whose photo did not come in, its message kept out of the report", async () => {
  onCapture = () =>
    window.setTimeout(
      () => emit("tether-capture-failed", { kind: "timeout", message: "DSCF7.RAF from 93X1234567 not at /Users/someone/x" }),
      5,
    );
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  expect(report()).toContain("| Connect | pass |  |");
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| FAIL \| 0\/5 in the library; shot not delivered \(timeout\) \|/);
  // The message is shown in the test, never copied.
  expect(screen.getByText("DSCF7.RAF from 93X1234567 not at /Users/someone/x")).toBeTruthy();
  expect(report()).not.toMatch(/DSCF7|93X1234567|someone/);
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

it("counts a RAW+JPEG exposure once, so missing exposures fail the burst", async () => {
  onCapture = (n) =>
    window.setTimeout(() => {
      if (n > 3) {
        emit("tether-capture-failed", { kind: "timeout", message: "The camera did not deliver the shot." });
        return;
      }
      for (const extension of ["RAF", "JPG"])
        emit("tether-captured", {
          cameraName: `DSCF${n}.${extension}`,
          connection: 1,
          exposure: n,
          destination: null,
          assetId: `a${n}${extension}`,
          skipped: false,
          error: null,
          requested: true,
        });
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3, 4]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Burst from Rawtonic \| FAIL \| 3\/5 in the library; \d+ ms; shot not delivered \(timeout\) \|/);
});

it("asks the camera for nothing more once the test is closed", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  onCapture = () => undefined;
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  fireEvent.click(screen.getByRole("button", { name: "Run" }));
  await waitFor(() => expect(shot).toBe(1));
  cleanup();
  // Past every wait a step could still be in.
  await vi.advanceTimersByTimeAsync(400_000);
  vi.useRealTimers();
  expect(shot).toBe(1);
  expect(invoke.mock.calls.filter(([command]) => command === "tether_capture")).toHaveLength(1);
});

it("runs under React's StrictMode (its effects set up twice)", async () => {
  render(
    <StrictMode>
      <TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />
    </StrictMode>,
  );
  await runStep(/\| Read settings \| pass \|/);
});

it("fails a step whose RAW was not added although its JPEG was", async () => {
  onCapture = (n) =>
    window.setTimeout(() => {
      for (const [extension, error] of [["JPG", null], ["RAF", "disk full at /Users/someone/x"]])
        emit("tether-captured", {
          cameraName: `DSCF${n}.${extension}`,
          connection: 1,
          exposure: n,
          destination: null,
          assetId: error ? null : `a${n}`,
          skipped: false,
          error,
          requested: true,
        });
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| FAIL \| 5\/5 in the library; .*5 files not added or shots not delivered \|/);
  expect(report()).toContain("- Files not added or shots not delivered during the test: 5");
  expect(report()).not.toMatch(/someone|disk full/);
});

it("a companion that fails to download, or fails after its step ended, fails that step", async () => {
  // The JPEG is in; its RAW's download fails (the helper reports the shot
  // as not delivered).
  onCapture = (n) =>
    window.setTimeout(() => {
      emit("tether-captured", {
        cameraName: `DSCF${n}.JPG`,
        connection: 1,
        exposure: n,
        destination: null,
        assetId: `a${n}`,
        skipped: false,
        error: null,
        requested: true,
      });
      emit("tether-capture-failed", { kind: "storage", message: "Writing DSCF.RAF failed", connection: 1, exposure: n });
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3, 4]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Burst from Rawtonic \| FAIL \| 5\/5 in the library; \d+ ms; 5 files not added or shots not delivered \|/);
  // A body shot passes; its RAW fails later, after the step.
  fireEvent.click(screen.getByRole("button", { name: "Run" }));
  await screen.findByText("Press the shutter button on the camera now.");
  emit("tether-captured", {
    cameraName: "DSCF50.JPG",
    connection: 1,
    exposure: 50,
    destination: null,
    assetId: "b",
    skipped: false,
    error: null,
    requested: false,
  });
  await waitFor(() => expect(report()).toContain("| Shot on the camera | pass |"), { timeout: 4000 });
  emit("tether-captured", {
    cameraName: "DSCF50.RAF",
    connection: 1,
    exposure: 50,
    destination: null,
    assetId: null,
    skipped: false,
    error: "disk full",
    requested: false,
  });
  expect(report()).toContain("| Shot on the camera | FAIL | 1/1 in the library; a file of its photos failed after the step |");
  expect(report()).toContain("- Files not added or shots not delivered during the test: 6");
}, 20_000);

it("a failure goes to the step whose exposure it belongs to, not the step running then", async () => {
  onCapture = (n) =>
    window.setTimeout(
      () =>
        emit("tether-captured", {
          cameraName: `DSCF${n}.JPG`,
          connection: 1,
          exposure: n,
          destination: null,
          assetId: `a${n}`,
          skipped: false,
          error: null,
          requested: true,
        }),
      5,
    );
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| pass \|/);
  // During the burst, the RAW of shot 2 fails: the shots step fails, the
  // burst does not.
  const base = onCapture;
  onCapture = (n) => {
    base(n);
    if (n === 6)
      window.setTimeout(
        () =>
          emit("tether-capture-failed", { kind: "storage", message: "DSCF2.RAF could not be saved", connection: 1, exposure: 2 }),
        5,
      );
  };
  await runStep(/\| Burst from Rawtonic \| pass \| 5\/5/);
  expect(report()).toMatch(/\| Shots from Rawtonic \| FAIL \| 5\/5 in the library; .*; a file of its photos failed after the step \|/);
  // During the transfer-queue step, a failure without an exposure goes to
  // the last photo step.
  for (const _ of [1, 2]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.click(screen.getByRole("button", { name: "Run" }));
  emit("tether-capture-failed", { kind: "timeout", message: "late", connection: null, exposure: null });
  await waitFor(() => expect(report()).toContain("| Burst from Rawtonic | FAIL |"), { timeout: 4000 });
}, 20_000);

it("a RAW that fails first keeps its exposure: its JPEG in the next step is no new shot", async () => {
  onCapture = (n) =>
    window.setTimeout(() => {
      const jpeg = (k: number) => ({
        cameraName: `DSCF${k}.JPG`,
        connection: 1,
        exposure: k,
        destination: null,
        assetId: `a${k}`,
        skipped: false,
        error: null,
        requested: true,
      });
      // Shot 1's RAW cannot be saved; its JPEG comes only during the burst.
      if (n === 1)
        emit("tether-capture-failed", { kind: "storage", message: "DSCF1.RAF could not be saved", connection: 1, exposure: 1 });
      else if (n === 6) {
        emit("tether-captured", jpeg(1));
        emit("tether-captured", jpeg(6));
      }
      // The burst's last shot never comes.
      else if (n === 10) emit("tether-capture-failed", { kind: "timeout", message: "never came", connection: null, exposure: null });
      else emit("tether-captured", jpeg(n));
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| FAIL \| 4\/5 in the library;/);
  await runStep(/\| Burst from Rawtonic \| FAIL \| 4\/5 in the library; \d+ ms; shot not delivered \(timeout\) \|/);
}, 20_000);

it("a companion's failure ends no other shot's wait", async () => {
  // Every shot: RAW in; its JPEG's download times out (with its exposure)
  // only after the next shot was asked for.
  let pendingJpegFailure: (() => void) | null = null;
  onCapture = (n) =>
    window.setTimeout(() => {
      pendingJpegFailure?.();
      emit("tether-captured", {
        cameraName: `DSCF${n}.RAF`,
        connection: 1,
        exposure: n,
        destination: null,
        assetId: `a${n}`,
        skipped: false,
        error: null,
        requested: true,
      });
      pendingJpegFailure = () =>
        emit("tether-capture-failed", { kind: "timeout", message: "JPG timed out", connection: 1, exposure: n });
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3, 4]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Burst from Rawtonic \| FAIL \| 5\/5 in the library; \d+ ms; 4 files not added or shots not delivered \|/);
  expect(shot).toBe(5);
}, 20_000);
