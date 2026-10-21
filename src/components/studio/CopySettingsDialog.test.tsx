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
  await act(() => result.current.handlePasteFromPrevious(["/s/B.ARW"]));
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
  useLibraryStore.setState({ multiSelectedPaths: ["/s/B.ARW"] });
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
  expect(paste.paths).toEqual(["/s/B.ARW"]);
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


it("shows on the open photo what the backend wrote for it, as one undoable step", async () => {
  const written = { ...structuredClone(INITIAL_ADJUSTMENTS), exposure: 2, masks: [dodge] };
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === Invokes.LoadMetadata) return { adjustments: written };
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2, masks: [sky] } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  const own = { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [dodge] } as unknown as Adjustments;
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own, history: [own], historyIndex: 0 });
  useLibraryStore.setState({ multiSelectedPaths: ["/s/B.ARW", "/s/C.ARW"] });
  await act(async () => {
    await result.current.handlePasteAdjustments();
  });
  // The open photo first, alone; then the others.
  expect(applied().map((call) => call.paths)).toEqual([["/s/B.ARW"], ["/s/C.ARW"]]);
  expect(useEditorStore.getState().adjustments.exposure).toBe(2);
  expect(useEditorStore.getState().history).toHaveLength(2);
  act(() => useEditorStore.getState().undo());
  expect(useEditorStore.getState().adjustments).toBe(own);
});

it("keeps an edit made on the open photo while the paste was written, and says so", async () => {
  let edit = () => {};
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === Invokes.ApplyAdjustmentsToPaths) edit();
    if (name === Invokes.LoadMetadata) return { adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), exposure: 2 } };
    return undefined;
  });
  const { toast } = await import("react-toastify");
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: structuredClone(INITIAL_ADJUSTMENTS) });
  const mine = { ...structuredClone(INITIAL_ADJUSTMENTS), contrast: 30 };
  edit = () => useEditorStore.setState({ adjustments: mine });
  await act(async () => {
    await result.current.handlePasteAdjustments(["/s/B.ARW"]);
  });
  expect(useEditorStore.getState().adjustments).toBe(mine);
  expect(toast.error).toHaveBeenCalled();
});

it("gives the open photo what loading gives it: a never-edited RAW's lens data, AI bitmaps under own ids", async () => {
  const written = { exposure: 2, masks: [sky] };
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === Invokes.LoadMetadata) return { adjustments: structuredClone(written) };
    if (name === Invokes.GetCameraLensProfile) return { params: { k1: 0.1 } };
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2, masks: [sky] } as unknown as Adjustments, copiedFrom: "/s/A.ARW?vc=c0ffee" });
  useEditorStore.setState({ selectedImage: photo("/s/A.ARW") as never, adjustments: structuredClone(INITIAL_ADJUSTMENTS) });
  await act(async () => {
    await result.current.handlePasteAdjustments(["/s/A.ARW"]);
  });
  const shown = useEditorStore.getState().adjustments;
  expect(shown.exposure).toBe(2);
  // Its recipe has no lens mode (never edited): the camera's lens data.
  expect(shown.lensCorrectionMode).toBe("camera");
  expect(shown.lensDistortionParams).toEqual({ k1: 0.1 });
  // The bitmap kept (same photograph), under an id the renderer has no other bitmap for.
  expect(shown.masks[0].subMasks[0].id).not.toBe("sky-c");
  expect(shown.masks[0].subMasks[0].parameters).toMatchObject({ maskDataBase64: "data:a" });
});

it("pastes onto the open photo's edit as it is now, and writes the others when showing it fails", async () => {
  const { debouncedSave } = await import("../../hooks/useEditorActions");
  const order: string[] = [];
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    const a = args as { paths?: string[]; adjustments?: Adjustments };
    if (name === Invokes.SaveMetadataAndUpdateThumbnail) order.push(`save:${a.adjustments?.contrast}`);
    if (name === Invokes.ApplyAdjustmentsToPaths) order.push(`paste:${a.paths?.join(",")}`);
    if (name === Invokes.LoadMetadata) throw new Error("gone");
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: structuredClone(INITIAL_ADJUSTMENTS) });
  // An edit not yet saved (its save still waits for the debounce).
  const edited = { ...structuredClone(INITIAL_ADJUSTMENTS), contrast: 30 };
  useEditorStore.setState({ adjustments: edited });
  debouncedSave("/s/B.ARW", edited);
  await act(async () => {
    await result.current.handlePasteAdjustments(["/s/B.ARW", "/s/C.ARW"]);
  });
  await act(async () => {});
  expect(order).toEqual(["save:30", "paste:/s/B.ARW", "paste:/s/C.ARW"]);
});

it("keeps the ids of the open photo's bitmaps a paste did not change", async () => {
  const own = { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [sky] } as unknown as Adjustments;
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === Invokes.LoadMetadata) return { adjustments: { ...structuredClone(own), exposure: 2, lensCorrectionMode: "manual" } };
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own });
  await act(async () => {
    await result.current.handlePasteAdjustments(["/s/B.ARW"]);
  });
  const shown = useEditorStore.getState().adjustments;
  expect(shown.exposure).toBe(2);
  expect(shown.masks[0].subMasks[0].id).toBe("sky-c");
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
  act(() => useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never }));
  await act(() => result.current.handlePasteFromPrevious(["/s/C.ARW"]));
  await act(async () => {});
  expect(applied()[0].adjustments).toMatchObject({ lensCorrectionMode: "camera", lensDistortionParams: { k1: 0.2 } });
});

it("pastes three times onto the open photo, each onto the result of the one before", async () => {
  let disk: Record<string, unknown> = { ...structuredClone(INITIAL_ADJUSTMENTS) };
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    const a = args as { adjustments?: Record<string, unknown> };
    if (name === Invokes.ApplyAdjustmentsToPaths) disk = { ...disk, ...a.adjustments };
    if (name === Invokes.SaveMetadataAndUpdateThumbnail) disk = { ...a.adjustments };
    if (name === Invokes.LoadMetadata) return { adjustments: structuredClone(disk) };
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: structuredClone(INITIAL_ADJUSTMENTS) });
  useEditorStore.setState({ copiedAdjustments: { exposure: 1 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  useEditorStore.setState({ copiedAdjustments: { contrast: 20 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  useEditorStore.setState({ copiedAdjustments: { saturation: -10 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  for (let i = 0; i < 12; i++) await act(async () => {});
  const shown = useEditorStore.getState().adjustments;
  expect([shown.exposure, shown.contrast, shown.saturation]).toEqual([1, 20, -10]);
  expect([disk.exposure, disk.contrast, disk.saturation]).toEqual([1, 20, -10]);
});

it("pastes three times onto the open photo with the editor's autosave running, without waiting on itself", async () => {
  const { debouncedSave, registerScheduledRecipe } = await import("../../hooks/useEditorActions");
  let disk: Record<string, unknown> = { ...structuredClone(INITIAL_ADJUSTMENTS) };
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    const a = args as { adjustments?: Record<string, unknown> };
    if (name === Invokes.ApplyAdjustmentsToPaths) disk = { ...disk, ...a.adjustments };
    if (name === Invokes.SaveMetadataAndUpdateThumbnail) disk = { ...a.adjustments };
    if (name === Invokes.LoadMetadata) return { adjustments: structuredClone(disk) };
    return undefined;
  });
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: structuredClone(INITIAL_ADJUSTMENTS) });
  // The editor's autosave (useImageProcessing): each new recipe handed to the
  // debounced save unless marked as handed over.
  let scheduled = { path: "/s/B.ARW", adjustments: useEditorStore.getState().adjustments };
  const unregister = registerScheduledRecipe(() => scheduled, (recipe) => (scheduled = recipe));
  const unsubscribe = useEditorStore.subscribe((state) => {
    queueMicrotask(() => {
      const now = useEditorStore.getState().adjustments;
      if (scheduled.adjustments !== now) {
        scheduled = { path: "/s/B.ARW", adjustments: now };
        debouncedSave("/s/B.ARW", now);
      }
    });
    void state;
  });
  const { result } = renderHook(useEditorActions);
  for (const copied of [{ exposure: 1 }, { contrast: 20 }, { saturation: -10 }]) {
    useEditorStore.setState({ copiedAdjustments: copied as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
    act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  }
  for (let i = 0; i < 20; i++) await act(async () => {});
  debouncedSave.flush();
  for (let i = 0; i < 5; i++) await act(async () => {});
  unsubscribe();
  unregister();
  const shown = useEditorStore.getState().adjustments;
  expect([shown.exposure, shown.contrast, shown.saturation]).toEqual([1, 20, -10]);
  expect([disk.exposure, disk.contrast, disk.saturation]).toEqual([1, 20, -10]);
});
