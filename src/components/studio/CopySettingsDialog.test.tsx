import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { CopySettingsDialogHost } from "./CopySettingsDialog";
import { useEditorActions } from "../../hooks/useEditorActions";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, PasteMode, type Adjustments } from "../../utils/adjustments";
import { Invokes, type AppSettings } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn() } }));

const sky = {
  ...structuredClone(INITIAL_MASK_CONTAINER),
  id: "sky",
  name: "Sky",
  subMasks: [{ id: "sky-c", type: "ai-sky", visible: true, invert: false, opacity: 100, mode: "additive", parameters: { maskDataBase64: "data:a" } }],
};
const dodge = { ...structuredClone(INITIAL_MASK_CONTAINER), id: "dodge", name: "Dodge", subMasks: [] };
const source: Adjustments = {
  ...structuredClone(INITIAL_ADJUSTMENTS),
  exposure: 1,
  contrast: 20,
  vignetteAmount: -30,
  masks: [sky, dodge],
} as unknown as Adjustments;
const photo = (path: string) => ({ path, isReady: true, isRaw: true, width: 6000, height: 4000, exif: {}, thumbnailUrl: "" });
const applied = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([name]) => name === Invokes.ApplyAdjustmentsToPaths)
    .map(([, args]) => args as { paths: string[]; adjustments: Partial<Adjustments>; paste?: { source: string; replaceLayers: boolean } });

beforeEach(() => {
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === Invokes.LoadMetadata) return { adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), exposure: -1, clarity: 15 } };
    return undefined;
  });
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    selectedImage: photo("/s/A.ARW") as never,
    adjustments: structuredClone(source),
  });
  useLibraryStore.setState({ multiSelectedPaths: ["/s/A.ARW"], libraryActivePath: "/s/A.ARW" });
  const handleSettingsChange = vi.fn(async (settings: AppSettings) => useSettingsStore.setState({ appSettings: settings }));
  useSettingsStore.setState({
    appSettings: { copyPasteSettings: { mode: PasteMode.Merge, includedAdjustments: ["exposure"], knownAdjustments: [], autoSync: false } } as unknown as AppSettings,
    handleSettingsChange,
  });
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("copies exactly the chosen tools and layers and remembers the choice", async () => {
  render(<CopySettingsDialogHost />);
  const { result } = renderHook(useEditorActions);
  await act(() => result.current.handleCopyAdjustments());
  expect(screen.getByRole("dialog", { name: "Copy settings" })).toBeTruthy();
  // Remembered: the group holding exposure; layers were not chosen.
  expect((screen.getByRole("checkbox", { name: /Exposure & Tone Mapper/ }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("checkbox", { name: "Sky AI selections are detected on each photo" }) as HTMLInputElement).checked).toBe(false);
  // Capture One's "adjusted tools": those this photo changed, all layers.
  fireEvent.click(screen.getByRole("button", { name: "Changed" }));
  expect((screen.getByRole("checkbox", { name: /^Tone/ }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("checkbox", { name: /^Grain/ }) as HTMLInputElement).checked).toBe(false);
  fireEvent.click(screen.getByRole("checkbox", { name: "Dodge" }));
  fireEvent.click(screen.getByRole("button", { name: "Copy" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  const { copiedAdjustments, copiedFrom } = useEditorStore.getState();
  expect(copiedFrom).toBe("/s/A.ARW");
  expect(copiedAdjustments).toMatchObject({ exposure: 1, contrast: 20, vignetteAmount: -30 });
  expect("clarity" in copiedAdjustments!).toBe(false);
  expect(copiedAdjustments!.masks!.map((layer) => layer.id)).toEqual(["sky"]);
  const remembered = useSettingsStore.getState().appSettings!.copyPasteSettings!.includedAdjustments;
  expect(remembered).toContain("contrast");
  expect(remembered).toContain("masks");
  // Processing versions are off unless chosen: none on the clipboard.
  expect("toneVersion" in copiedAdjustments!).toBe(false);

  // Paste: onto the selection, from that photograph (layers join theirs).
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: structuredClone(INITIAL_ADJUSTMENTS) });
  act(() => result.current.handlePasteAdjustments(["/s/C.ARW"]));
  await act(async () => {});
  const [paste] = applied();
  expect(paste.paths).toEqual(["/s/C.ARW"]);
  expect(paste.paste).toMatchObject({ source: "/s/A.ARW", replaceLayers: false, newEdit: { raw: expect.any(Object) } });
  expect(paste.adjustments).toMatchObject({ exposure: 1, contrast: 20 });
  expect("sharpenVersion" in paste.adjustments).toBe(false);
  expect(paste.adjustments.masks?.[0].subMasks[0].parameters).toMatchObject({ maskDataBase64: "data:a" });
});

it("syncs the open photo's chosen settings to the other selected photographs", async () => {
  useLibraryStore.setState({ multiSelectedPaths: ["/s/A.ARW", "/s/B.ARW", "/s/C.ARW"] });
  render(<CopySettingsDialogHost />);
  const { result } = renderHook(useEditorActions);
  await act(() => result.current.handleSyncSettings());
  expect(screen.getByRole("dialog", { name: "Sync settings to the other selected photos (2)" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Sync (2)" }));
  await act(async () => {});
  expect(applied()).toEqual([
    {
      paths: ["/s/B.ARW", "/s/C.ARW"],
      adjustments: expect.objectContaining({ exposure: 1 }),
      paste: expect.objectContaining({ source: "/s/A.ARW", replaceLayers: false }),
    },
  ]);
  expect("contrast" in applied()[0].adjustments).toBe(false);
  // Option: the choice of last time, without the picker.
  await act(() => result.current.handleSyncSettings({ quick: true }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(applied()).toHaveLength(2);
});

it("pastes the previous photograph's settings, as last chosen", async () => {
  const { result } = renderHook(useEditorActions);
  // A.ARW open, then B.ARW: A is the previous one.
  act(() => useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never }));
  expect(useEditorStore.getState().previousPhoto).toBe("/s/A.ARW");
  await act(() => result.current.handlePasteFromPrevious(["/s/C.ARW"]));
  await act(async () => {});
  // A's saved recipe (not the open edit), as remembered: exposure only.
  expect(vi.mocked(invoke)).toHaveBeenCalledWith(Invokes.LoadMetadata, { path: "/s/A.ARW" });
  const [paste] = applied();
  expect(paste.adjustments).toMatchObject({ exposure: -1 });
  expect("clarity" in paste.adjustments).toBe(false);
  expect(paste.paste).toMatchObject({ source: "/s/A.ARW", replaceLayers: false });
});

it("Previous reads the previous photograph's edit after its save, and pastes where it was asked", async () => {
  const { debouncedSave } = await import("../../hooks/useEditorActions");
  let failSave = () => {};
  let saves = 0;
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    // A's save fails; B's (saved as shown before the paste) goes through.
    if (name === Invokes.SaveMetadataAndUpdateThumbnail && ++saves === 1)
      return new Promise((_, reject) => (failSave = () => reject(new Error("disk full"))));
    if (name === Invokes.LoadMetadata) return { adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), exposure: -1 } };
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  // A's last edit is still being saved when B opens.
  debouncedSave("/s/A.ARW", { ...structuredClone(INITIAL_ADJUSTMENTS), exposure: 4 });
  debouncedSave.flush();
  act(() => useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never }));
  useLibraryStore.setState({ multiSelectedPaths: ["/s/D.ARW"] });
  let done!: Promise<void>;
  act(() => {
    done = result.current.handlePasteFromPrevious();
  });
  // Meanwhile another photo is selected; the paste still goes to B.
  useLibraryStore.setState({ multiSelectedPaths: ["/s/C.ARW"] });
  await act(async () => failSave());
  await act(() => done);
  await act(async () => {});
  await act(async () => {});
  const [paste] = applied();
  expect(paste.paths).toEqual(["/s/D.ARW"]);
  // The edit the failed save kept, not the older saved recipe.
  expect(paste.adjustments.exposure).toBe(4);
});

it("takes the keyboard while open and gives it back", async () => {
  render(
    <>
      <button>Workspace control</button>
      <CopySettingsDialogHost />
    </>,
  );
  const control = screen.getByRole("button", { name: "Workspace control" });
  control.focus();
  const { result } = renderHook(useEditorActions);
  await act(() => result.current.handleCopyAdjustments());
  const dialog = screen.getByRole("dialog", { name: "Copy settings" });
  expect(document.activeElement).toBe(dialog);
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(control);
});

it("remembers the layers chosen on a photograph and None clears the versions too", async () => {
  render(<CopySettingsDialogHost />);
  const { result } = renderHook(useEditorActions);
  await act(() => result.current.handleCopyAdjustments());
  fireEvent.click(screen.getByRole("checkbox", { name: "Sky AI selections are detected on each photo" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "This photo's processing versions" }));
  fireEvent.click(screen.getByRole("button", { name: "Copy" }));
  // Again on the same photo: Sky only, not Dodge.
  await act(() => result.current.handleCopyAdjustments());
  expect((screen.getByRole("checkbox", { name: "Sky AI selections are detected on each photo" }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("checkbox", { name: "Dodge" }) as HTMLInputElement).checked).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "None" }));
  expect((screen.getByRole("checkbox", { name: "This photo's processing versions" }) as HTMLInputElement).checked).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  // Option-copy honours the same layers.
  await act(() => result.current.handleCopyAdjustments(undefined, { quick: true }));
  expect(useEditorStore.getState().copiedAdjustments!.masks!.map((layer) => layer.id)).toEqual(["sky"]);
});

it("takes the processing versions only when they are chosen", async () => {
  render(<CopySettingsDialogHost />);
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ adjustments: { ...structuredClone(source), toneVersion: 2 } as Adjustments });
  await act(() => result.current.handleCopyAdjustments());
  const versions = screen.getByRole("checkbox", { name: "This photo's processing versions" }) as HTMLInputElement;
  expect(versions.checked).toBe(false);
  fireEvent.click(versions);
  fireEvent.click(screen.getByRole("button", { name: "Copy" }));
  expect(useEditorStore.getState().copiedAdjustments).toMatchObject({ toneVersion: 2, exposure: 1 });
  // Remembered: on next time.
  await act(() => result.current.handleCopyAdjustments());
  expect((screen.getByRole("checkbox", { name: "This photo's processing versions" }) as HTMLInputElement).checked).toBe(true);
});

it("leaves the processing versions out of quick copies when preferences list them without the choice", async () => {
  // Preferences from before the picker: every copyable key, versions too.
  useSettingsStore.setState({
    appSettings: {
      copyPasteSettings: { mode: PasteMode.Merge, includedAdjustments: ["exposure", "toneVersion", "sharpenVersion"], knownAdjustments: [], autoSync: false },
    } as unknown as AppSettings,
  });
  useEditorStore.setState({ adjustments: { ...structuredClone(source), toneVersion: 2 } as Adjustments });
  const { result } = renderHook(useEditorActions);
  await act(() => result.current.handleCopyAdjustments(undefined, { quick: true }));
  expect(useEditorStore.getState().copiedAdjustments).toMatchObject({ exposure: 1 });
  expect("toneVersion" in useEditorStore.getState().copiedAdjustments!).toBe(false);
});

it("remembers the previous photograph across a closed editor", () => {
  act(() => useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never }));
  act(() => useEditorStore.setState({ selectedImage: null }));
  act(() => useEditorStore.setState({ selectedImage: photo("/s/C.ARW") as never }));
  expect(useEditorStore.getState().previousPhoto).toBe("/s/B.ARW");
});


it("Previous takes a never-edited RAW's settings as the editor showed them", async () => {
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === Invokes.LoadMetadata) return { adjustments: null };
    if (name === Invokes.GetCameraLensProfile) return { params: { k1: 0.2 } };
    return undefined;
  });
  useSettingsStore.setState({
    appSettings: { copyPasteSettings: { mode: PasteMode.Merge, includedAdjustments: ["lensCorrectionMode", "lensDistortionParams"], knownAdjustments: [], autoSync: false } } as unknown as AppSettings,
    // A RAW by its extension (A is not in the shown list).
    supportedTypes: { raw: ["arw"], nonRaw: ["jpg"] },
  });
  const { result } = renderHook(useEditorActions);
  // E (never edited) open, then B.
  act(() => useEditorStore.setState({ selectedImage: photo("/s/E.ARW") as never }));
  act(() => useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never }));
  await act(() => result.current.handlePasteFromPrevious(["/s/C.ARW"]));
  await act(async () => {});
  expect(applied()[0].adjustments).toMatchObject({ lensCorrectionMode: "camera", lensDistortionParams: { k1: 0.2 } });
});

it("Previous frames drawn masks with the size the previous photo was shown at", async () => {
  const radial = {
    ...structuredClone(INITIAL_MASK_CONTAINER),
    id: "vignette",
    name: "Vignette",
    subMasks: [{ id: "r", type: "radial", visible: true, invert: false, opacity: 100, mode: "additive", parameters: { centerX: 3000, centerY: 2000 } }],
  };
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === Invokes.LoadMetadata) return { adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [radial] } };
    return undefined;
  });
  useSettingsStore.setState({
    appSettings: { copyPasteSettings: { mode: PasteMode.Merge, includedAdjustments: ["masks"], knownAdjustments: [], autoSync: false } } as unknown as AppSettings,
  });
  const { result } = renderHook(useEditorActions);
  // P shown at 6000 × 4000, then Q.
  act(() => useEditorStore.setState({ selectedImage: photo("/s/P.ARW") as never }));
  act(() => useEditorStore.setState({ selectedImage: { ...photo("/s/Q.ARW"), width: 3000, height: 2000 } as never }));
  await act(() => result.current.handlePasteFromPrevious(["/s/R.ARW"]));
  await act(async () => {});
  const frame = (applied()[0].adjustments.masks?.[0].subMasks[0].parameters as { frame?: { width?: number; height?: number } }).frame;
  expect([frame?.width, frame?.height]).toEqual([6000, 4000]);
});

it("pastes onto the open photo at once, as one undoable edit, the others in the backend", () => {
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2, masks: [sky] } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const own = { ...opened, clarity: 7, masks: [dodge] } as unknown as Adjustments;
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own, history: [opened, own], historyIndex: 1 });
  useLibraryStore.setState({ multiSelectedPaths: ["/s/B.ARW", "/s/C.ARW"] });
  act(() => result.current.handlePasteAdjustments());
  expect(applied().map((call) => call.paths)).toEqual([["/s/C.ARW"]]);
  const shown = useEditorStore.getState().adjustments;
  expect([shown.exposure, shown.clarity]).toEqual([2, 7]);
  // Its own layer kept, the pasted one added, its detection its own.
  expect(shown.masks.map((layer) => layer.id)).toEqual(["dodge", "sky"]);
  expect(shown.masks[1].subMasks[0].parameters).toMatchObject({ maskDataBase64: null, detectPending: true });
  expect(shown.masks[1].subMasks[0].id).not.toBe("sky-c");
  expect(useEditorStore.getState().history).toHaveLength(3);
  act(() => useEditorStore.getState().undo());
  expect(useEditorStore.getState().adjustments).toBe(own);
});

it("pastes onto the open photo's saved recipe while it is not edited, as loading makes it", async () => {
  const { rememberOpenedRecipe } = await import("../../hooks/useEditorActions");
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  // A never-edited RAW with camera lens data, as it was opened.
  rememberOpenedRecipe("/s/F.ARW", { saved: null, camera: { params: { k1: 0.1 } as never }, width: 6000, height: 4000 });
  const own = structuredClone(INITIAL_ADJUSTMENTS);
  useEditorStore.setState({ selectedImage: photo("/s/F.ARW") as never, adjustments: own, history: [own], historyIndex: 0, historyPending: false });
  act(() => result.current.handlePasteAdjustments(["/s/F.ARW"]));
  const shown = useEditorStore.getState().adjustments;
  expect(shown.exposure).toBe(2);
  expect(shown.lensCorrectionMode).toBe("camera");
  expect(shown.lensDistortionParams).toEqual({ k1: 0.1 });
  expect(shown.aspectRatio).toBe(1.5);
});

it("keeps the ids of the open photo's bitmaps a paste did not change, and its own bitmap on a virtual copy", () => {
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const own = { ...opened, masks: [sky] } as unknown as Adjustments;
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own, history: [opened, own], historyIndex: 1 });
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  expect(useEditorStore.getState().adjustments.masks[0].subMasks[0].id).toBe("sky-c");
  // Pasted from its virtual copy: the same pixels, the bitmap stays.
  useEditorStore.setState({ copiedAdjustments: { masks: [sky] } as unknown as Adjustments, copiedFrom: "/s/B.ARW?vc=c0ffee" });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  expect(useEditorStore.getState().adjustments.masks[0].subMasks[0].parameters).toMatchObject({ maskDataBase64: "data:a" });
});

it("refuses a paste onto the open photo beyond the layers the renderer applies", async () => {
  const { toast } = await import("react-toastify");
  const { result } = renderHook(useEditorActions);
  const many = Array.from({ length: 32 }, (_, i) => ({ ...dodge, id: `l${i}`, subMasks: [{ ...sky.subMasks[0], id: `c${i}`, type: "radial" }] }));
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const own = { ...opened, masks: [many[0]] } as unknown as Adjustments;
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own, history: [opened, own], historyIndex: 1 });
  useEditorStore.setState({ copiedAdjustments: { masks: many.slice(1).concat([{ ...many[0], id: "extra" }]) } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  expect(useEditorStore.getState().adjustments).toBe(own);
  expect(toast.error).toHaveBeenCalled();
});

it("resolves a pasted lens choice for the open photo first, then pastes in one step", async () => {
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.GetCameraLensProfile) return { params: { k1: 0.3 } };
    if (name === Invokes.GetLensDistortionParams) return { k1: (args as { model: string }).model === "50mm" ? 0.5 : 0 };
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const own = { ...opened, clarity: 1, lensCorrectionMode: "manual", lensDistortionParams: { k1: 0.9 } } as unknown as Adjustments;
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own, history: [opened, own], historyIndex: 1 });
  // The camera's own data.
  useEditorStore.setState({
    copiedAdjustments: { exposure: 1, lensCorrectionMode: "camera", lensDistortionParams: { k1: 7 } } as unknown as Adjustments,
    copiedFrom: "/s/A.ARW",
  });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  for (let i = 0; i < 3; i++) await act(async () => {});
  let shown = useEditorStore.getState().adjustments;
  expect([shown.exposure, shown.lensCorrectionMode, shown.lensDistortionParams]).toEqual([1, "camera", { k1: 0.3 }]);
  expect(useEditorStore.getState().history).toHaveLength(3);
  // A chosen lens: its profile for this photo's EXIF.
  useEditorStore.setState({
    copiedAdjustments: { lensCorrectionMode: "manual", lensMaker: "Maker", lensModel: "50mm", lensDistortionParams: { k1: 7 } } as unknown as Adjustments,
  });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  for (let i = 0; i < 3; i++) await act(async () => {});
  shown = useEditorStore.getState().adjustments;
  expect([shown.lensModel, shown.lensDistortionParams]).toEqual(["50mm", { k1: 0.5 }]);
  expect(useEditorStore.getState().history).toHaveLength(4);
  act(() => useEditorStore.getState().undo());
  expect(useEditorStore.getState().adjustments.lensDistortionParams).toEqual({ k1: 0.3 });
});

it("pastes a lens choice as onto a closed photo when its photo is left while its lens data is read", async () => {
  let release = () => {};
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === Invokes.GetCameraLensProfile) {
      await new Promise<void>((resolve) => (release = resolve));
      return { params: { k1: 0.3 } };
    }
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const own = { ...opened, clarity: 1 } as unknown as Adjustments;
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own, history: [opened, own], historyIndex: 1 });
  useEditorStore.setState({ copiedAdjustments: { lensCorrectionMode: "camera" } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  act(() => useEditorStore.setState({ selectedImage: photo("/s/C.ARW") as never }));
  await act(async () => {});
  await act(async () => release());
  for (let i = 0; i < 4; i++) await act(async () => {});
  expect(applied()).toEqual([expect.objectContaining({ paths: ["/s/B.ARW"], adjustments: { lensCorrectionMode: "camera" } })]);
});

it("keeps pastes onto the open photo in order behind a lens lookup, and quitting waits for them", async () => {
  const { flushRecipeSaves } = await import("../../hooks/useEditorActions");
  let release = () => {};
  let asked: number | undefined;
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.GetCameraLensProfile) {
      asked = (args as { caVersion: number }).caVersion;
      await new Promise<void>((resolve) => (release = resolve));
      return { params: { k1: 0.3 } };
    }
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const own = { ...opened, clarity: 1 } as unknown as Adjustments;
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own, history: [opened, own], historyIndex: 1 });
  // A camera lens choice from a recipe without a camera CA version (0), exposure 1; then exposure 2.
  useEditorStore.setState({
    copiedAdjustments: { exposure: 1, lensCorrectionMode: "camera", cameraCaVersion: null } as unknown as Adjustments,
    copiedFrom: "/s/A.ARW",
  });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  let quit = false;
  void flushRecipeSaves().then(() => (quit = true));
  await act(async () => {});
  expect(quit).toBe(false);
  await act(async () => release());
  for (let i = 0; i < 6; i++) await act(async () => {});
  const shown = useEditorStore.getState().adjustments;
  expect([shown.exposure, shown.lensDistortionParams]).toEqual([2, { k1: 0.3 }]);
  // Missing as on the source (0), as a closed photo gets it.
  expect(asked).toBe(0);
  expect("cameraCaVersion" in shown).toBe(false);
  expect(quit).toBe(true);
});

it("resolves a pasted lens with the open photo's EXIF also before its decode brought it", async () => {
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.ReadExifForPaths) return { "/s/B.ARW": { Make: "Maker", FocalLength: "85" } };
    if (name === Invokes.GetLensDistortionParams) return { k1: Number((args as { exif: Record<string, string> }).exif.FocalLength) };
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const own = { ...opened, clarity: 1 } as unknown as Adjustments;
  // Shown from its preview proxy: no EXIF in the editor yet.
  useEditorStore.setState({ selectedImage: { ...photo("/s/B.ARW"), exif: null } as never, adjustments: own, history: [opened, own], historyIndex: 1 });
  useEditorStore.setState({
    copiedAdjustments: { lensCorrectionMode: "manual", lensMaker: "Maker", lensModel: "85mm" } as unknown as Adjustments,
    copiedFrom: "/s/A.ARW",
  });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  for (let i = 0; i < 6; i++) await act(async () => {});
  expect(useEditorStore.getState().adjustments.lensDistortionParams).toEqual({ k1: 85 });
});

