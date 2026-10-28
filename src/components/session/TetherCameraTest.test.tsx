import "../../i18n";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import TetherCameraTest, { type TestedCamera } from "./TetherCameraTest";
import { useUIStore } from "../../store/useUIStore";

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

const APP = { version: "9.9.9", os: "macos", osVersion: "26.0", arch: "aarch64" };
let relay = true;
let iso = "160";
let isoChoices = ["160", "200"];
/** What the mock camera reports after a set: the value asked for, unless
 * it keeps or chooses another. */
let isoAfterSet = (asked: string) => asked;
let shot = 0;
let numbered = false;
/** What the mock camera does when fired: a photo in the library, its
 * Develop frame, and (on a camera with a transfer queue) its release. */
let onCapture: (n: number) => void;

beforeEach(() => {
  relay = true;
  iso = "160";
  isoChoices = ["160", "200"];
  isoAfterSet = (asked) => asked;
  // New photos show in Develop, where the photo on screen is timed.
  useUIStore.setState({ activeView: "editor" });
  shot = 0;
  numbered = false;
  onCapture = (n) => {
    const destination = `/Users/someone/Jobs/Acme/Capture/DSCF${n}.RAF`;
    window.setTimeout(() => {
      emit("tether-captured", {
        cameraName: `DSCF${n}.RAF`,
        connection: 1,
        exposure: n,
        destination,
        assetId: `a${n}`,
        skipped: false,
        error: null,
        requested: true,
      });
      emit("tether-released", { cameraName: `DSCF${n}.RAF`, connection: 1, exposure: n, freed: true, error: null });
      emit("wgpu-frame-ready", { path: destination });
    }, 5);
  };
  invoke.mockImplementation((command: string, args?: { value?: string }) => {
    switch (command) {
      case "tether_settings":
        return Promise.resolve(SETTINGS.map((s) => ({ ...s, value: iso, choices: isoChoices })));
      case "tether_set_setting":
        // The helper answers with the value it read back.
        iso = isoAfterSet(args?.value ?? iso);
        return Promise.resolve(iso);
      case "tether_capture":
        shot += 1;
        onCapture(shot);
        // The helper's request number, when the test's events name it
        // (else none: shots are matched by the order of their exposures).
        return Promise.resolve(numbered ? shot : null);
      case "contribution_app_info":
        return Promise.resolve(APP);
      case "contribution_available":
        return Promise.resolve(relay);
      case "contribution_prepare": {
        const { channel, payload } = args as unknown as { channel: string; payload: unknown };
        return Promise.resolve({ channel, body: JSON.stringify(payload, null, 2), digest: "d" });
      }
      case "contribution_send":
        return Promise.resolve({ id: "ID1", ref: "r" });
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

const report = () => (screen.getByRole("textbox", { name: "Report" }) as HTMLTextAreaElement).value;
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
  await runStep(/\| Change a setting \| pass \| ISO 160 → 200 changed, read back and set back/);
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
  // A photo left from an earlier run is no shot on the body.
  emit("tether-captured", {
    cameraName: "DSCF98.RAF",
    destination: "/x/DSCF98.RAF",
    assetId: "c",
    skipped: false,
    error: null,
    requested: false,
    connection: null,
    exposure: null,
  });
  await new Promise((done) => setTimeout(done, 50));
  expect(report()).not.toContain("| Shot on the camera | pass");
  emit("tether-captured", {
    cameraName: "DSCF99.RAF",
    connection: 1,
    exposure: 99,
    destination: "/x/DSCF99.RAF",
    assetId: "b",
    skipped: false,
    error: null,
    requested: false,
  });
  emit("tether-released", { cameraName: "DSCF99.RAF", connection: 1, exposure: 99, freed: true, error: null });
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
  // A late companion's release goes wrong: the step fails.
  emit("tether-captured", { cameraName: "DSCF99.JPG", connection: 1, exposure: 99, destination: null, assetId: "j", skipped: false, error: null, requested: false });
  emit("tether-released", { cameraName: "DSCF99.JPG", connection: 1, exposure: 99, freed: false, error: "the camera refused" });
  expect(report()).toContain("| Transfer queue freed after commit | FAIL | 12 freed; a later release failed |");
  const text = report();
  expect(text).toContain("- Firmware: 4.10");
  expect(text).toContain("- Transfer queue: yes (freeing on)");
  expect(text).toContain("- System: macos 26.0 (aarch64)");
  expect(text).not.toMatch(/93X1234567|someone|Acme|DSCF|\/Users/);
}, 20_000);

it("sends the result only after the review, as the relay's camera-test payload", async () => {
  render(<TetherCameraTest camera={CAMERA} connectMs={2310} freeCameraCopies onClose={vi.fn()} />);
  await waitFor(() => expect(report()).toContain("- Rawtonic: 9.9.9"));
  fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  const send = await screen.findByRole("button", { name: "Send result…" });
  await waitFor(() => expect((send as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(send);
  fireEvent.click(await screen.findByRole("button", { name: "Review and send…" }));
  await screen.findByText(/This is exactly what Rawtonic will send/);
  expect(invoke.mock.calls.some(([command]) => command === "contribution_send")).toBe(false);
  const prepared = invoke.mock.calls.find(([command]) => command === "contribution_prepare")?.[1] as {
    channel: string;
    payload: { camera: unknown; report: string; app: unknown; schema: number };
  };
  expect(prepared.channel).toBe("cameraTest");
  expect(prepared.payload).toEqual({
    schema: 1,
    app: APP,
    camera: { make: "Fuji", model: "Fuji Fujifilm X-H2S", firmware: "4.10" },
    report: report(),
  });
  expect(JSON.stringify(prepared.payload)).not.toMatch(/93X1234567/);
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await screen.findByText("Sent. Thank you!");
  expect(screen.getByTestId("report-id").textContent).toBe("ID1");
});

it("without the relay shares the result: GitHub's camera report form, prefilled after showing its text", async () => {
  relay = false;
  render(<TetherCameraTest camera={CAMERA} connectMs={2310} freeCameraCopies onClose={vi.fn()} />);
  await waitFor(() => expect(report()).toContain("- Rawtonic: 9.9.9"));
  const share = await screen.findByRole("button", { name: "Share the result…" });
  await waitFor(() => expect((share as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(share);
  await screen.findByText(/cannot send reports itself/);
  expect(screen.queryByRole("button", { name: "Review and send…" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Open on GitHub" }));
  await screen.findByText(/exactly this title and text/);
  expect(openUrl).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Open in browser" }));
  await waitFor(() => expect(openUrl).toHaveBeenCalledTimes(1));
  const url = new URL(openUrl.mock.calls[0][0] as string);
  expect(url.origin + url.pathname).toBe("https://github.com/rawtonic-app/rawtonic/issues/new");
  expect(url.searchParams.get("template")).toBe("camera-report.yml");
  expect(url.searchParams.get("title")).toBe("Camera report: Fuji Fujifilm X-H2S");
  expect(url.searchParams.get("report")).toBe(report());
  expect(url.searchParams.get("body")).toBeNull();
});

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
  await runStep(/\| Shots from Rawtonic \| FAIL \| 0\/5 in the library; no photo within 60 s of the shot \(not taken, or not handed over\) \|/);
  // The message is shown in the test, never copied.
  expect(screen.getByText("DSCF7.RAF from 93X1234567 not at /Users/someone/x")).toBeTruthy();
  expect(report()).not.toMatch(/DSCF7|93X1234567|someone/);
});

it("guides a Canon tester step by step and counts each shot not received in the report", async () => {
  onCapture = (n) =>
    window.setTimeout(() => {
      if (n === 2) emit("tether-capture-failed", { kind: "timeout", message: "Shot 2 was not received." });
      else
        emit("tether-captured", {
          cameraName: `IMG_${n}.CR3`,
          connection: 1,
          exposure: n,
          destination: null,
          assetId: `a${n}`,
          skipped: false,
          error: null,
          requested: true,
        });
    }, 5);
  const canon: TestedCamera = { ...CAMERA, model: "Canon EOS R6", transferQueue: false, experimental: true };
  render(<TetherCameraTest camera={canon} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  expect(screen.getByText("How to test your Canon camera")).toBeTruthy();
  expect(screen.getByText("Keep a memory card in the camera for the whole test.")).toBeTruthy();
  expect(await screen.findByText(/At the end, click “Send result…”/)).toBeTruthy();
  expect(report()).toContain("- Driver path: experimental (Canon EOS");
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| FAIL \| 4\/5 in the library/);
  expect(report()).toContain("- Shots not received (on the memory card if the camera took them): 1");
  // The send dialog says the result is from the experimental path.
  fireEvent.click(screen.getByRole("button", { name: "Send result…" }));
  expect(await screen.findByText(/This result comes from experimental Canon support/)).toBeTruthy();
  cleanup();
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  expect(screen.queryByText("How to test your Canon camera")).toBeNull();
  expect(report()).not.toContain("Driver path");
});

it("finds a release before its photo's commit and skips the queue step on cameras without one", async () => {
  onCapture = (n) =>
    window.setTimeout(() => {
      emit("tether-released", { cameraName: `DSCF${n}.RAF`, connection: 1, exposure: n, freed: true, error: null });
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
  await runStep(/\| Burst from Rawtonic \| FAIL \| 3\/5 in the library; \d+ ms; no photo within 60 s of the shot \(not taken, or not handed over\) \|/);
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
  await runStep(/\| Burst from Rawtonic \| FAIL \| 4\/5 in the library; \d+ ms; no photo within 60 s of the shot \(not taken, or not handed over\) \|/);
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

it("a step with a photo whose exposure cannot be told is inconclusive, never pass or fail", async () => {
  // The camera reuses one name: its later JPEGs come marked ambiguous.
  onCapture = (n) =>
    window.setTimeout(() => {
      emit("tether-captured", {
        cameraName: "DSC00001.ARW",
        connection: 1,
        exposure: n,
        ambiguous: [],
        destination: null,
        assetId: `a${n}`,
        skipped: false,
        error: null,
        requested: true,
      });
      if (n === 2)
        emit("tether-captured", {
          cameraName: "DSC00001.JPG",
          connection: 1,
          exposure: 2,
          ambiguous: [1, 2],
          destination: null,
          assetId: "j",
          skipped: false,
          error: null,
          requested: true,
        });
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(
    /\| Shots from Rawtonic \| inconclusive \| 5\/5 in the library; .*; the camera reused a file name, so its RAW and JPEG files could not be matched to shots \|/,
  );
  // A late ambiguous file of a passed step makes it inconclusive too.
  await runStep(/\| Burst from Rawtonic \| pass \|/);
  emit("tether-captured", {
    cameraName: "DSC00001.JPG",
    connection: 1,
    exposure: 7,
    ambiguous: [6, 7],
    destination: null,
    assetId: "k",
    skipped: false,
    error: null,
    requested: true,
  });
  expect(report()).toMatch(/\| Burst from Rawtonic \| inconclusive \|/);
}, 20_000);

it("an ambiguous file that fails makes its step inconclusive, and earlier-run photos wait for no release", async () => {
  onCapture = (n) =>
    window.setTimeout(() => {
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
      emit("tether-released", { cameraName: `DSCF${n}.RAF`, connection: 1, exposure: n, freed: true, error: null });
      if (n === 3)
        emit("tether-capture-failed", {
          kind: "storage",
          message: "DSC00001.JPG could not be saved",
          connection: 1,
          exposure: 3,
          ambiguous: [2, 3],
        });
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  // A photo left from an earlier run comes in meanwhile (no release follows).
  emit("tether-captured", {
    cameraName: "OLD1.RAF",
    connection: null,
    exposure: null,
    destination: null,
    assetId: "o",
    skipped: false,
    error: null,
    requested: false,
  });
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| inconclusive \|/);
  expect(report()).toContain("- Files not added or shots not delivered during the test: 1");
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Transfer queue freed after commit \| pass \| 5 freed \|/);
}, 20_000);

it("a photo of a shot from before the test does not answer a test shot", async () => {
  numbered = true;
  onCapture = (n) =>
    window.setTimeout(() => {
      if (n === 1) {
        // A late file of a shot fired before the test opened (request 77),
        // then the test's first shot never comes.
        emit("tether-captured", {
          cameraName: "DSC09000.JPG",
          connection: 1,
          exposure: 900,
          ambiguous: [],
          request: 77,
          destination: null,
          assetId: "old",
          skipped: false,
          error: null,
          requested: true,
        });
        emit("tether-capture-failed", { kind: "timeout", message: "never came", request: 1, connection: null, exposure: null, ambiguous: [] });
        return;
      }
      emit("tether-captured", {
        cameraName: `DSCF${n}.RAF`,
        connection: 1,
        exposure: 900 + n,
        ambiguous: [],
        request: n,
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
  await runStep(/\| Shots from Rawtonic \| FAIL \| 4\/5 in the library;/);
}, 20_000);

it("an ambiguous file makes every step it may belong to inconclusive", async () => {
  numbered = true;
  onCapture = (n) =>
    window.setTimeout(() => {
      emit("tether-captured", {
        cameraName: "DSC00001.ARW",
        connection: 1,
        exposure: n,
        ambiguous: [],
        request: n,
        destination: null,
        assetId: `a${n}`,
        skipped: false,
        error: null,
        requested: true,
      });
      // During the burst: a JPEG under the reused name fails; it may be
      // shot 2's (Shots) or shot 7's (Burst).
      if (n === 7)
        emit("tether-capture-failed", { kind: "storage", message: "JPG not saved", request: 7, connection: 1, exposure: 7, ambiguous: [2, 7] });
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| pass \|/);
  await runStep(/\| Burst from Rawtonic \| inconclusive \|/);
  expect(report()).toMatch(/\| Shots from Rawtonic \| inconclusive \|/);
}, 20_000);

it("a failure of a request the test did not send fails no step", async () => {
  numbered = true;
  onCapture = (n) =>
    window.setTimeout(() => {
      emit("tether-captured", {
        cameraName: `DSCF${n}.RAF`,
        connection: 1,
        exposure: n,
        ambiguous: [],
        request: n,
        destination: null,
        assetId: `a${n}`,
        skipped: false,
        error: null,
        requested: true,
      });
      // A shot fired from the panel before the test opened (request 77)
      // times out without an exposure while the test's shots run.
      if (n === 2)
        emit("tether-capture-failed", { kind: "timeout", message: "never came", request: 77, connection: null, exposure: null, ambiguous: [] });
    }, 5);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| pass \| 5\/5 in the library;/);
  // Counted in the header only; also once no step runs.
  expect(report()).toContain("- Files not added or shots not delivered during the test: 1");
  emit("tether-capture-failed", { kind: "timeout", message: "never came", request: 78, connection: null, exposure: null, ambiguous: [] });
  expect(report()).toContain("- Files not added or shots not delivered during the test: 2");
  expect(report()).toMatch(/\| Shots from Rawtonic \| pass \|/);
}, 20_000);

it("a release of a photo committed before the test is none of the test's", async () => {
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  // Committed before the test opened: its release comes now.
  emit("tether-released", { cameraName: "DSCF50.RAF", connection: 1, exposure: 50, freed: true, error: null });
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| pass \| 5\/5/);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Transfer queue freed after commit \| pass \| 5 freed \|/);
  // Another such release after the step leaves it passed.
  emit("tether-released", { cameraName: "DSCF51.RAF", connection: 1, exposure: 51, freed: false, error: "gone" });
  expect(report()).toContain("| Transfer queue freed after commit | pass | 5 freed |");
}, 20_000);

it("changes ISO from Auto to a plain number and sets Auto back", async () => {
  // A Sony list: Auto ISO first, extended ISO 50, Multi Frame NR values.
  iso = "Auto ISO";
  isoChoices = ["Auto ISO", "50", "100", "400", "800", "Auto ISO Multi Frame Noise Reduction", "100 Multi Frame Noise Reduction"];
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Change a setting \| pass \| ISO Auto ISO → 100 changed, read back and set back/);
  expect(iso).toBe("Auto ISO");
  const sets = invoke.mock.calls.filter(([command]) => command === "tether_set_setting").map(([, args]) => args.value);
  expect(sets).toEqual(["100", "Auto ISO"]);
});

it("says when the camera kept its value or chose another, and sets it back", async () => {
  iso = "Auto ISO";
  isoChoices = ["Auto ISO", "100", "400"];
  isoAfterSet = (asked) => (asked === "Auto ISO" ? asked : iso);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Change a setting \| FAIL \| \d+ ms; ISO: the camera kept Auto ISO when set to 100 \|/);
  cleanup();
  iso = "100";
  isoAfterSet = (asked) => (asked === "400" ? "320" : asked);
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  await runStep(/\| Change a setting \| FAIL \| \d+ ms; ISO: set to 400 \(from 100\), the camera chose 320 \|/);
  expect(iso).toBe("100");
});

it("says why the photo on screen was not timed", async () => {
  useUIStore.setState({ activeView: "library" });
  render(<TetherCameraTest camera={CAMERA} connectMs={null} freeCameraCopies onClose={vi.fn()} />);
  for (const _ of [1, 2, 3]) fireEvent.click(screen.getByRole("button", { name: "Skip" }));
  fireEvent.change(screen.getByLabelText("Number of shots"), { target: { value: "5" } });
  await runStep(/\| Shots from Rawtonic \| pass \| 5\/5 in the library; photo on screen not timed: Develop was not showing; in library median/);
});

