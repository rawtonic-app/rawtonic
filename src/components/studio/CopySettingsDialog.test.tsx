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

it("pastes onto the open photo with its own layers kept and the AI detection its own", async () => {
  const { result } = renderHook(useEditorActions);
  useEditorStore.setState({ copiedAdjustments: { exposure: 2, masks: [sky] } as unknown as Adjustments, copiedFrom: "/s/A.ARW" });
  useEditorStore.setState({
    selectedImage: photo("/s/B.ARW") as never,
    adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), masks: [dodge] } as unknown as Adjustments,
  });
  act(() => result.current.handlePasteAdjustments(["/s/B.ARW"]));
  const { adjustments } = useEditorStore.getState();
  expect(adjustments.exposure).toBe(2);
  expect(adjustments.masks.map((layer) => layer.id)).toEqual(["dodge", "sky"]);
  expect(adjustments.masks[1].subMasks[0].parameters).toMatchObject({ maskDataBase64: null, detectPending: true });
  // On a virtual copy of the same photograph the bitmap is its own.
  useEditorStore.setState({ selectedImage: photo("/s/A.ARW?vc=c0ffee") as never, adjustments: structuredClone(INITIAL_ADJUSTMENTS) });
  act(() => result.current.handlePasteAdjustments(["/s/A.ARW?vc=c0ffee"]));
  expect(useEditorStore.getState().adjustments.masks[0].subMasks[0].parameters).toMatchObject({ maskDataBase64: "data:a" });
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
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === Invokes.SaveMetadataAndUpdateThumbnail)
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
