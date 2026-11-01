import "../../i18n";
// @ts-expect-error node:fs is untyped in the frontend tsconfig
import { readFileSync } from "node:fs";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { notify } from "../../activity/notify";
import { CameraPreview, CameraPreviewButton, useCameraPreviewGuard } from "./CameraPreview";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useMultiViewStore } from "../../store/useMultiViewStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import { useRelightStore } from "../../store/useRelightStore";
import { Panel, type AppSettings, type ImageFile, type SelectedImage } from "../ui/AppProperties";
import { INITIAL_ADJUSTMENTS, type Adjustments } from "../../utils/adjustments";
import {
  hideCameraPreview,
  showCameraPreview,
  toggleCameraPreview,
} from "../../utils/cameraPreview";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(() => Promise.resolve(null)),
}));
vi.mock("../../activity/notify", () => ({
  notify: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}));

const raw = (path: string, extra: Partial<SelectedImage> = {}) =>
  ({
    path,
    width: 6000,
    height: 4000,
    isRaw: true,
    isReady: true,
    ...extra,
  }) as unknown as SelectedImage;

/** The native display's answers: whether it shows the camera JPEG. */
function nativeShows(shown: boolean | (() => Promise<boolean>)) {
  vi.mocked(invoke).mockImplementation(((command: string) =>
    command === "show_camera_preview"
      ? typeof shown === "function" ? shown() : Promise.resolve(shown)
      : Promise.resolve(null)) as never);
}
const calls = (command: string) => vi.mocked(invoke).mock.calls.filter(([name]) => name === command);

function GuardProbe() {
  useCameraPreviewGuard();
  return null;
}

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: "macos" });
  useUIStore.setState({ activeView: "editor", activePanel: Panel.Adjustments });
  useMultiViewStore.setState({ active: false, libraryEnabled: false });
  useLibraryStore.setState({
    imageList: ["/p/a.dng", "/p/b.dng"].map((path) => ({ path, is_raw: true, is_cloud_placeholder: false }) as ImageFile),
  });
  useEditorStore.setState({
    selectedImage: raw("/p/a.dng"),
    adjustments: { ...INITIAL_ADJUSTMENTS },
    history: [{ ...INITIAL_ADJUSTMENTS }],
    historyIndex: 0,
    splitView: false,
    showOriginal: false,
    toolBefore: null,
    cameraPreview: null,
    isWbPickerActive: false,
    colorPickerTarget: null,
    tatTarget: null,
    lumaPickerLayer: null,
    isGuidedPerspectiveActive: false,
  });
  useEditorStore.setState((s) => ({ faceProtect: { ...s.faceProtect, painting: false } }));
  useRelightStore.setState({ panelOpen: false });
  // No show or hide of an earlier test is still wanted.
  hideCameraPreview();
  vi.clearAllMocks();
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("switches the native camera preview on and off with the button, its shortcut in the tooltip", async () => {
  nativeShows(true);
  render(<CameraPreviewButton />);
  const button = screen.getByRole("button", { name: "Camera preview" });
  expect(button.getAttribute("aria-pressed")).toBe("false");
  expect(button.getAttribute("title")).toMatch(/^Camera preview \(.*\\\)$/);
  fireEvent.click(button);
  await vi.waitFor(() => expect(useEditorStore.getState().cameraPreview).toEqual({ path: "/p/a.dng" }));
  expect(calls("show_camera_preview")).toEqual([["show_camera_preview", { path: "/p/a.dng" }]]);
  expect(button.getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(button);
  expect(useEditorStore.getState().cameraPreview).toBeNull();
  expect(calls("hide_camera_preview")).toHaveLength(1);
});

it("is disabled with a tooltip that says so when the photo has no camera JPEG", async () => {
  // A photograph without one: not RAW (the JPEG lives in a RAW file).
  useLibraryStore.setState({ imageList: [{ path: "/p/a.jpg", is_raw: false } as ImageFile] });
  useEditorStore.setState({ selectedImage: raw("/p/a.jpg", { isRaw: false }) });
  const { rerender } = render(<CameraPreviewButton />);
  let button = screen.getByRole("button", { name: "Camera preview" });
  expect((button as HTMLButtonElement).disabled).toBe(true);
  expect(button.getAttribute("title")).toBe("This photo has no camera preview.");
  await act(async () => {
    expect(await showCameraPreview()).toEqual({ shown: false, missing: false });
  });
  expect(invoke).not.toHaveBeenCalled();

  // A RAW the native side finds no camera JPEG in keeps the edit.
  useEditorStore.setState({ selectedImage: raw("/p/b.dng") });
  nativeShows(false);
  rerender(<CameraPreviewButton />);
  button = screen.getByRole("button", { name: "Camera preview" });
  expect((button as HTMLButtonElement).disabled).toBe(false);
  await act(async () => {
    expect(await showCameraPreview()).toEqual({ shown: false, missing: true });
  });
  expect(useEditorStore.getState().cameraPreview).toBeNull();
  // The toggle says so instead of doing nothing.
  act(() => toggleCameraPreview());
  await vi.waitFor(() => expect(notify.info).toHaveBeenCalledWith("This photo has no camera preview."));
});

it("leaves the recipe and the history untouched", async () => {
  nativeShows(true);
  const recipe = { ...INITIAL_ADJUSTMENTS, exposure: 0.75, contrast: 12 } as Adjustments;
  useEditorStore.setState({
    adjustments: recipe,
    history: [{ ...INITIAL_ADJUSTMENTS }, recipe],
    historyIndex: 1,
  });
  const before = useEditorStore.getState();
  await act(async () => {
    await showCameraPreview();
  });
  expect(useEditorStore.getState().cameraPreview?.path).toBe("/p/a.dng");
  const shown = useEditorStore.getState();
  expect(shown.adjustments).toBe(before.adjustments);
  expect(shown.history).toBe(before.history);
  expect(shown.historyIndex).toBe(before.historyIndex);
  act(() => hideCameraPreview());
  const after = useEditorStore.getState();
  expect(after.adjustments).toBe(before.adjustments);
  expect(after.history).toBe(before.history);
});

it("stepping to the next photograph ends the preview: never the old photo's JPEG", async () => {
  nativeShows(true);
  render(<GuardProbe />);
  await act(async () => {
    await showCameraPreview();
  });
  expect(useEditorStore.getState().cameraPreview?.path).toBe("/p/a.dng");
  act(() => useEditorStore.setState({ selectedImage: raw("/p/b.dng", { isReady: false }) }));
  expect(useEditorStore.getState().cameraPreview).toBeNull();
  expect(calls("hide_camera_preview")).toHaveLength(1);
  // And it does not come back by itself for the next one.
  act(() => useEditorStore.setState({ selectedImage: raw("/p/b.dng") }));
  expect(useEditorStore.getState().cameraPreview).toBeNull();
  expect(calls("show_camera_preview")).toHaveLength(1);
});

it("a camera JPEG that arrives after a step is taken away, not shown for the next photograph", async () => {
  let answer!: (shown: boolean) => void;
  nativeShows(() => new Promise((resolve) => (answer = resolve)));
  render(<GuardProbe />);
  const pending = showCameraPreview();
  act(() => useEditorStore.setState({ selectedImage: raw("/p/b.dng") }));
  await act(async () => {
    answer(true);
    expect(await pending).toEqual({ shown: false, missing: false });
  });
  expect(useEditorStore.getState().cameraPreview).toBeNull();
  expect(calls("hide_camera_preview")).toHaveLength(1);
});

it("the key again while the camera JPEG is on its way cancels it", async () => {
  let answer!: (shown: boolean) => void;
  nativeShows(() => new Promise((resolve) => (answer = resolve)));
  act(() => toggleCameraPreview());
  act(() => toggleCameraPreview());
  await act(async () => {
    answer(true);
  });
  await vi.waitFor(() => expect(calls("hide_camera_preview")).toHaveLength(2));
  expect(useEditorStore.getState().cameraPreview).toBeNull();
});

it("a late answer of a cancelled show never hides a newer show's preview", async () => {
  const answers: Array<(shown: boolean) => void> = [];
  nativeShows(() => new Promise((resolve) => answers.push(resolve)));
  // On, off and on again before the first answer.
  act(() => toggleCameraPreview());
  act(() => toggleCameraPreview());
  act(() => toggleCameraPreview());
  expect(answers).toHaveLength(2);
  expect(calls("hide_camera_preview")).toHaveLength(1);
  // The newer one answers first, then the cancelled one.
  await act(async () => {
    answers[1](true);
  });
  await vi.waitFor(() => expect(useEditorStore.getState().cameraPreview?.path).toBe("/p/a.dng"));
  await act(async () => {
    answers[0](true);
  });
  await act(async () => {});
  expect(calls("hide_camera_preview")).toHaveLength(1);
  expect(useEditorStore.getState().cameraPreview?.path).toBe("/p/a.dng");
});

it.each([
  ["a tool that edits (masks)", () => useUIStore.getState().setPanel(Panel.Masks)],
  ["the crop tool", () => useUIStore.getState().setPanel(Panel.Crop)],
  ["leaving Develop", () => useUIStore.setState({ activeView: "library" })],
  ["the split view", () => useEditorStore.setState({ splitView: true })],
  ["a held tool reset (per-tool before)", () => useEditorStore.setState({ toolBefore: "Exposure" })],
  ["the multi view", () => useMultiViewStore.setState({ active: true })],
  ["the colour picker", () => useEditorStore.setState({ colorPickerTarget: "hsl" })],
  ["a Levels picker", () => useEditorStore.setState({ tatTarget: "levelsWhite" })],
  ["targeted adjustment", () => useEditorStore.setState({ tatTarget: "curve" })],
  ["the luminance picker", () => useEditorStore.setState({ lumaPickerLayer: "layer" })],
  ["the protection brush", () =>
    useEditorStore.setState((s) => ({ faceProtect: { ...s.faceProtect, painting: true } }))],
  ["Relight's handles", () => useRelightStore.setState({ panelOpen: true, showHandles: true })],
  ["guided perspective", () => useEditorStore.setState({ isGuidedPerspectiveActive: true })],
])("%s brings the edit back through the native hide", async (_, leave) => {
  nativeShows(true);
  render(<GuardProbe />);
  await act(async () => {
    await showCameraPreview();
  });
  expect(useEditorStore.getState().cameraPreview).not.toBeNull();
  act(leave);
  expect(useEditorStore.getState().cameraPreview).toBeNull();
  expect(calls("hide_camera_preview")).toHaveLength(1);
});

it("the viewer going away ends it", async () => {
  nativeShows(true);
  const { unmount } = render(<GuardProbe />);
  await act(async () => {
    await showCameraPreview();
  });
  unmount();
  expect(useEditorStore.getState().cameraPreview).toBeNull();
  expect(calls("hide_camera_preview")).toHaveLength(1);
});

it("cannot switch on with another view of the photo or a tool that edits", async () => {
  nativeShows(true);
  useEditorStore.setState({ splitView: true });
  await act(async () => {
    expect(await showCameraPreview()).toEqual({ shown: false, missing: false });
  });
  useEditorStore.setState({ splitView: false, isWbPickerActive: true });
  await act(async () => {
    expect(await showCameraPreview()).toEqual({ shown: false, missing: false });
  });
  expect(invoke).not.toHaveBeenCalled();
});

it("labels the native camera JPEG and says how to leave it, without an image of its own", async () => {
  nativeShows(true);
  const { container } = render(<CameraPreview />);
  expect(container.querySelector('[data-bench-id="camera-preview"]')).toBeNull();
  await act(async () => {
    await showCameraPreview();
  });
  const overlay = container.querySelector('[data-bench-id="camera-preview"]')!;
  // The native display draws the JPEG: no webview image over the edit.
  expect(overlay.querySelector("img")).toBeNull();
  expect(overlay.textContent).toContain("Camera preview");
  expect(overlay.textContent).toContain("not your edit");
  expect(overlay.textContent).toMatch(/\\ or Esc returns/);
  act(() => toggleCameraPreview());
  expect(container.querySelector('[data-bench-id="camera-preview"]')).toBeNull();
});

it("keeps the system colours of the forced-colours rules for its note", () => {
  const css = readFileSync("src/components/studio/studio.css", "utf8") as string;
  const forced = css.slice(css.indexOf("@media (forced-colors: active)", css.indexOf(".studio-camera-preview-note")));
  const rules = [...forced.matchAll(/([^{}]+)\{([^{}]*)\}/g)];
  const rule = (selector: string) =>
    rules.find(([, selectors]) => selectors.split(",").some((s) => s.trim() === selector))?.[2] ?? "";
  expect(rule(".studio-camera-preview-note")).toContain("border-color: CanvasText");
  expect(rule(".studio-camera-preview-note")).toContain("color: CanvasText");
  expect(rule(".studio-camera-preview-note")).not.toMatch(/#|rgba?\(/);
});
