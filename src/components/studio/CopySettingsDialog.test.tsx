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

/** The backend's paste onto a base recipe (`paste_preview`): here a plain merge. */
const previewMerge = (args: unknown) => {
  const a = args as { base: Record<string, unknown> | null; adjustments: Record<string, unknown> };
  return { ...(a.base ?? {}), ...a.adjustments };
};
const previews = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([name]) => name === Invokes.PastePreview)
    .map(([, args]) => args as { path: string; base: Record<string, unknown> | null });

it("pastes onto the open photo as one undoable edit, the others in the backend", async () => {
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.PastePreview) return previewMerge(args);
    if (name === Invokes.LoadMetadata) return { adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), clarity: 7 } };
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  const own = { ...structuredClone(INITIAL_ADJUSTMENTS), clarity: 7 } as unknown as Adjustments;
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own, history: [own], historyIndex: 0, historyPending: false });
  useLibraryStore.setState({ multiSelectedPaths: ["/s/B.ARW", "/s/C.ARW"] });
  act(() => result.current.handlePasteAdjustments());
  for (let i = 0; i < 6; i++) await act(async () => {});
  expect(applied().map((call) => call.paths)).toEqual([["/s/C.ARW"]]);
  // Not edited since opening: pasted onto its saved recipe.
  expect(previews()[0].base).toMatchObject({ clarity: 7 });
  const shown = useEditorStore.getState().adjustments;
  expect([shown.exposure, shown.clarity]).toEqual([2, 7]);
  expect(useEditorStore.getState().history).toHaveLength(2);
  act(() => useEditorStore.getState().undo());
  expect(useEditorStore.getState().adjustments).toBe(own);
});

it("pastes onto the open photo's edit as it is now, and again when it is edited meanwhile", async () => {
  let editMeanwhile: (() => void) | null = null;
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.PastePreview) {
      const merged = previewMerge(args);
      editMeanwhile?.();
      editMeanwhile = null;
      return merged;
    }
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const edited = { ...opened, contrast: 30 };
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: edited, history: [opened, edited], historyIndex: 1 });
  editMeanwhile = () => useEditorStore.setState({ adjustments: { ...useEditorStore.getState().adjustments, saturation: -5 } });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  for (let i = 0; i < 8; i++) await act(async () => {});
  // Edited since opening: pasted onto that edit, and onto the next one.
  expect(previews().map((p) => p.base?.contrast)).toEqual([30, 30]);
  expect(previews()[1].base?.saturation).toBe(-5);
  const shown = useEditorStore.getState().adjustments;
  expect([shown.exposure, shown.contrast, shown.saturation]).toEqual([2, 30, -5]);
});

it("gives the open photo what loading gives it: a never-edited RAW's lens data, changed AI bitmaps under own ids", async () => {
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.PastePreview) return { ...previewMerge({ ...(args as object), base: {} }), masks: [sky] };
    if (name === Invokes.LoadMetadata) return { adjustments: null };
    if (name === Invokes.GetCameraLensProfile) return { params: { k1: 0.1 } };
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments, copiedFrom: "/s/F.ARW?vc=c0ffee" });
  const own = structuredClone(INITIAL_ADJUSTMENTS);
  useEditorStore.setState({ selectedImage: photo("/s/F.ARW") as never, adjustments: own, history: [own], historyIndex: 0, historyPending: false });
  act(() => result.current.handlePasteAdjustments(["/s/F.ARW"]));
  for (let i = 0; i < 6; i++) await act(async () => {});
  expect(previews()[0].base).toBeNull();
  const shown = useEditorStore.getState().adjustments;
  expect(shown.exposure).toBe(2);
  expect(shown.lensCorrectionMode).toBe("camera");
  expect(shown.lensDistortionParams).toEqual({ k1: 0.1 });
  expect(shown.masks[0].subMasks[0].id).not.toBe("sky-c");
  expect(shown.masks[0].subMasks[0].parameters).toMatchObject({ maskDataBase64: "data:a" });
});

it("keeps the ids of the open photo's bitmaps a paste did not change", async () => {
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.PastePreview) return previewMerge(args);
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const own = { ...opened, lensCorrectionMode: "manual", masks: [sky] } as unknown as Adjustments;
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: own, history: [opened, own], historyIndex: 1 });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  for (let i = 0; i < 6; i++) await act(async () => {});
  const shown = useEditorStore.getState().adjustments;
  expect(shown.exposure).toBe(2);
  expect(shown.masks[0].subMasks[0].id).toBe("sky-c");
});

it("pastes three times quickly onto the open photo, each onto the one before", async () => {
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.PastePreview) return previewMerge(args);
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const edited = { ...opened, clarity: 1 };
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: edited, history: [opened, edited], historyIndex: 1 });
  for (const copied of [{ exposure: 1 }, { contrast: 20 }, { saturation: -10 }]) {
    useEditorStore.setState({ copiedAdjustments: copied as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
    act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  }
  for (let i = 0; i < 20; i++) await act(async () => {});
  const shown = useEditorStore.getState().adjustments;
  expect([shown.exposure, shown.contrast, shown.saturation, shown.clarity]).toEqual([1, 20, -10, 1]);
});

it("applies quick pastes onto the open photo in the order they were asked for, whatever answers first", async () => {
  const answers: (() => void)[] = [];
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.PastePreview) {
      const merged = previewMerge(args);
      await new Promise<void>((resolve) => answers.push(resolve));
      return merged;
    }
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const edited = { ...opened, clarity: 1 };
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: edited, history: [opened, edited], historyIndex: 1 });
  for (const exposure of [1, 2]) {
    useEditorStore.setState({ copiedAdjustments: { exposure } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
    act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  }
  for (let i = 0; i < 4; i++) await act(async () => {});
  // One at a time: the second is asked for only after the first is applied.
  expect(answers).toHaveLength(1);
  await act(async () => answers[0]());
  for (let i = 0; i < 4; i++) await act(async () => {});
  await act(async () => answers[1]());
  for (let i = 0; i < 4; i++) await act(async () => {});
  expect(useEditorStore.getState().adjustments.exposure).toBe(2);
});

it("pastes onto the photo it was asked for when another is opened before it is applied, and quitting waits", async () => {
  const { flushRecipeSaves } = await import("../../hooks/useEditorActions");
  let answer = () => {};
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.PastePreview) {
      const merged = previewMerge(args);
      await new Promise<void>((resolve) => (answer = resolve));
      return merged;
    }
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const edited = { ...opened, clarity: 1 };
  useEditorStore.setState({ selectedImage: photo("/s/B.ARW") as never, adjustments: edited, history: [opened, edited], historyIndex: 1 });
  useEditorStore.setState({ copiedAdjustments: { exposure: 3 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  for (let i = 0; i < 3; i++) await act(async () => {});
  act(() => useEditorStore.setState({ selectedImage: photo("/s/C.ARW") as never }));
  let quit = false;
  void flushRecipeSaves().then(() => (quit = true));
  await act(async () => {});
  expect(quit).toBe(false);
  await act(async () => answer());
  for (let i = 0; i < 6; i++) await act(async () => {});
  expect(applied()).toEqual([expect.objectContaining({ paths: ["/s/B.ARW"], adjustments: { exposure: 3 } })]);
  expect(quit).toBe(true);
});

it("makes opening the photo again wait for a paste onto it still being made", async () => {
  const { waitForRecipe } = await import("../../hooks/useEditorActions");
  let answer = () => {};
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.PastePreview) {
      const merged = previewMerge(args);
      await new Promise<void>((resolve) => (answer = resolve));
      return merged;
    }
    return undefined;
  });
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const edited = { ...opened, clarity: 1 };
  useEditorStore.setState({ selectedImage: photo("/s/G.ARW") as never, adjustments: edited, history: [opened, edited], historyIndex: 1 });
  useEditorStore.setState({ copiedAdjustments: { exposure: 3 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  act(() => result.current.handlePasteAdjustments(["/s/G.ARW"]));
  for (let i = 0; i < 3; i++) await act(async () => {});
  // G is left and opened again (still loading) before the paste is applied.
  act(() => useEditorStore.setState({ selectedImage: photo("/s/H.ARW") as never }));
  act(() => useEditorStore.setState({ selectedImage: { ...photo("/s/G.ARW"), isReady: false } as never }));
  let read = false;
  void Promise.resolve(waitForRecipe("/s/G.ARW")).then(() => (read = true));
  await act(async () => {});
  expect(read).toBe(false);
  await act(async () => answer());
  for (let i = 0; i < 6; i++) await act(async () => {});
  // Written as onto a closed photo, then read.
  expect(applied()).toEqual([expect.objectContaining({ paths: ["/s/G.ARW"] })]);
  expect(read).toBe(true);
});

it("Previous takes a photo left while a paste onto it was made with that paste", async () => {
  let answer = () => {};
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    if (name === Invokes.PastePreview) {
      const merged = previewMerge(args);
      await new Promise<void>((resolve) => (answer = resolve));
      return merged;
    }
    if (name === Invokes.LoadMetadata) return { adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), exposure: pastedYet ? 3 : 0 } };
    if (name === Invokes.ApplyAdjustmentsToPaths) pastedYet = true;
    return undefined;
  });
  let pastedYet = false;
  useSettingsStore.setState({
    appSettings: { copyPasteSettings: { mode: PasteMode.Merge, includedAdjustments: ["exposure"], knownAdjustments: [], autoSync: false } } as unknown as AppSettings,
  });
  const { result } = renderHook(useEditorActions);
  const opened = structuredClone(INITIAL_ADJUSTMENTS);
  const edited = { ...opened, clarity: 1 };
  useEditorStore.setState({ selectedImage: photo("/s/J.ARW") as never, adjustments: edited, history: [opened, edited], historyIndex: 1 });
  useEditorStore.setState({ copiedAdjustments: { exposure: 3 } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  act(() => result.current.handlePasteAdjustments(["/s/J.ARW"]));
  for (let i = 0; i < 3; i++) await act(async () => {});
  // J left before the paste is applied; then Previous (J) onto K.
  act(() => useEditorStore.setState({ selectedImage: photo("/s/K.ARW") as never }));
  let done!: Promise<void>;
  act(() => {
    done = result.current.handlePasteFromPrevious(["/s/L.ARW"]);
  });
  await act(async () => answer());
  await act(() => done);
  for (let i = 0; i < 4; i++) await act(async () => {});
  const ontoL = applied().find((call) => call.paths[0] === "/s/L.ARW");
  expect(ontoL?.adjustments.exposure).toBe(3);
});

