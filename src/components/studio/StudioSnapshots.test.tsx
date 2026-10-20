import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import StudioSnapshots from "./StudioSnapshots";
import StudioHistory from "./StudioHistory";
import { debouncedSetHistory, useEditorStore } from "../../store/useEditorStore";
import { useSnapshotStore, type Snapshot } from "../../store/useSnapshotStore";
import { INITIAL_ADJUSTMENTS, type Adjustments } from "../../utils/adjustments";
import type { SelectedImage } from "../ui/AppProperties";

/** The backend's snapshots, by photograph. */
let kept: Record<string, Snapshot[]> = {};
let thumbnailCalls: string[] = [];
/** When set, thumbnails wait for it (a slow render). */
let thumbnailGate: Promise<void> | null = null;
/** When set, renames wait for it (a slow answer). */
let renameGate: Promise<void> | null = null;
let nextId = 0;

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const recipe = (exposure: number): Adjustments => ({ ...structuredClone(INITIAL_ADJUSTMENTS), exposure });
const PHOTO = "/qa/photo.dng";

beforeEach(() => {
  kept = {};
  thumbnailCalls = [];
  thumbnailGate = null;
  renameGate = null;
  nextId = 0;
  vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: vi.fn(() => `blob:${++nextId}`), revokeObjectURL: vi.fn() }));
  vi.mocked(invoke).mockImplementation(async (name: string, args?: unknown) => {
    const a = args as { path: string; id?: string; name?: string; adjustments?: Adjustments };
    const list = (kept[a.path] ??= []);
    switch (name) {
      case "snapshots_list":
        return structuredClone(list);
      case "snapshot_create":
        list.push({ id: `s${list.length + 1}`, name: a.name!.trim(), createdAt: "2026-10-20T10:00:00.000Z", adjustments: a.adjustments! });
        return structuredClone(list);
      case "snapshot_rename": {
        list.find((s) => s.id === a.id)!.name = a.name!;
        // The list as it was when the rename ran, answered late.
        const answer = structuredClone(list);
        if (renameGate) await renameGate;
        return answer;
      }
      case "snapshot_update":
        list.find((s) => s.id === a.id)!.adjustments = a.adjustments!;
        return structuredClone(list);
      case "snapshot_delete":
        kept[a.path] = list.filter((s) => s.id !== a.id);
        return structuredClone(kept[a.path]);
      case "snapshot_thumbnail":
        thumbnailCalls.push(String(a.adjustments?.exposure));
        if (thumbnailGate) await thumbnailGate;
        return new ArrayBuffer(4);
      default:
        return undefined;
    }
  });
  useSnapshotStore.setState(useSnapshotStore.getInitialState());
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: recipe(0),
    selectedImage: { path: PHOTO, width: 6000, height: 4000, isRaw: true, isReady: true } as SelectedImage,
  });
  useEditorStore.getState().resetHistory(recipe(0));
});
afterEach(() => {
  cleanup();
  debouncedSetHistory.cancel();
  vi.unstubAllGlobals();
});

const openTool = (title: string) => {
  const details = document.querySelector<HTMLDetailsElement>(`[data-tool-title="${title}"]`)!;
  details.open = true;
};

it("takes a snapshot of the current edit, lists it with its thumbnail and renames it", async () => {
  useEditorStore.setState({ adjustments: recipe(1) });
  render(<StudioSnapshots />);
  await waitFor(() => expect(useSnapshotStore.getState().path).toBe(PHOTO));
  openTool("Snapshots");
  fireEvent.click(screen.getByRole("button", { name: "New snapshot of the current edit" }));
  await waitFor(() => expect(kept[PHOTO]).toHaveLength(1));
  expect(kept[PHOTO][0].adjustments.exposure).toBe(1);
  // Named as taken now; the name is selected for typing over at once.
  const input = await screen.findByRole("textbox", { name: "Snapshot name" });
  fireEvent.change(input, { target: { value: "One stop" } });
  fireEvent.keyDown(input, { key: "Enter" });
  await waitFor(() => expect(kept[PHOTO][0].name).toBe("One stop"));
  await waitFor(() => expect(document.querySelector(".studio-snapshot-thumb img")?.getAttribute("src")).toBe("blob:1"));
  expect(thumbnailCalls).toEqual(["1"]);
});

it("restores a snapshot as one named, undoable history step", async () => {
  kept[PHOTO] = [{ id: "a", name: "Bright", createdAt: "2026-10-20T10:00:00.000Z", adjustments: recipe(2) }];
  render(
    <>
      <StudioSnapshots />
      <StudioHistory />
    </>,
  );
  await screen.findByText("Bright");
  fireEvent.click(screen.getByTitle("Restore Bright (a history step: Undo returns)"));
  const editor = useEditorStore.getState();
  expect(editor.adjustments.exposure).toBe(2);
  expect(editor.history).toHaveLength(2);
  expect(editor.historyPending).toBe(false);
  openTool("History");
  expect(await screen.findByTitle("Snapshot Bright")).toBeTruthy();
  act(() => useEditorStore.getState().undo());
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
});

it("takes a snapshot of any history step, named after it, and deletes one", async () => {
  useEditorStore.getState().pushHistory(recipe(0.5));
  useEditorStore.getState().pushHistory(recipe(1.5));
  render(
    <>
      <StudioSnapshots />
      <StudioHistory />
    </>,
  );
  await waitFor(() => expect(useSnapshotStore.getState().path).toBe(PHOTO));
  openTool("History");
  fireEvent.click(screen.getByRole("button", { name: "Snapshot of this step: Exposure +0.50" }));
  await waitFor(() => expect(kept[PHOTO]).toHaveLength(1));
  expect(kept[PHOTO][0]).toMatchObject({ name: "Exposure +0.50", adjustments: { exposure: 0.5 } });
  // The edit is untouched by taking one.
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
  fireEvent.click(await screen.findByRole("button", { name: "Delete snapshot Exposure +0.50" }));
  await waitFor(() => expect(kept[PHOTO]).toHaveLength(0));
  expect(await screen.findByText(/No snapshots yet/)).toBeTruthy();
});

it("shows the next photograph's snapshots and releases the previous thumbnails", async () => {
  kept[PHOTO] = [{ id: "a", name: "First", createdAt: "2026-10-20T10:00:00.000Z", adjustments: recipe(1) }];
  kept["/qa/other.dng"] = [{ id: "b", name: "Second", createdAt: "2026-10-20T10:00:00.000Z", adjustments: recipe(3) }];
  render(<StudioSnapshots />);
  await screen.findByText("First");
  await waitFor(() => expect(useSnapshotStore.getState().thumbnails.a).toBe("blob:1"));
  act(() =>
    useEditorStore.setState({
      selectedImage: { path: "/qa/other.dng", width: 10, height: 10, isRaw: true, isReady: true } as SelectedImage,
    }),
  );
  await screen.findByText("Second");
  expect(screen.queryByText("First")).toBeNull();
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:1");
  // A restore never reaches a photograph the list does not belong to.
  useSnapshotStore.setState({ path: PHOTO });
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
});

it("updates a snapshot with the current edit and develops its thumbnail again", async () => {
  kept[PHOTO] = [{ id: "a", name: "Look", createdAt: "2026-10-20T10:00:00.000Z", adjustments: recipe(1) }];
  render(<StudioSnapshots />);
  await waitFor(() => expect(useSnapshotStore.getState().thumbnails.a).toBe("blob:1"));
  useEditorStore.setState({ adjustments: recipe(-1) });
  fireEvent.click(screen.getByRole("button", { name: "Update Look with the current edit" }));
  await waitFor(() => expect(useSnapshotStore.getState().thumbnails.a).toBe("blob:2"));
  expect(kept[PHOTO][0].adjustments.exposure).toBe(-1);
  expect(thumbnailCalls).toEqual(["1", "-1"]);
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:1");
});

it("restores after an edit still settling, which stays its own step", async () => {
  kept[PHOTO] = [{ id: "a", name: "Bright", createdAt: "2026-10-20T10:00:00.000Z", adjustments: recipe(2) }];
  render(<StudioSnapshots />);
  await screen.findByText("Bright");
  const edited = recipe(1);
  useEditorStore.setState({ adjustments: edited });
  debouncedSetHistory(edited);
  fireEvent.click(screen.getByTitle("Restore Bright (a history step: Undo returns)"));
  expect(useEditorStore.getState().history.map((step) => step.exposure)).toEqual([0, 1, 2]);
  act(() => useEditorStore.getState().undo());
  expect(useEditorStore.getState().adjustments.exposure).toBe(1);
});

it("drops a thumbnail developed from adjustments the snapshot no longer holds", async () => {
  kept[PHOTO] = [{ id: "a", name: "Look", createdAt: "2026-10-20T10:00:00.000Z", adjustments: recipe(1) }];
  let release = () => {};
  thumbnailGate = new Promise<void>((resolve) => (release = resolve));
  render(<StudioSnapshots />);
  await waitFor(() => expect(thumbnailCalls).toEqual(["1"]));
  useEditorStore.setState({ adjustments: recipe(2) });
  fireEvent.click(screen.getByRole("button", { name: "Update Look with the current edit" }));
  await waitFor(() => expect(kept[PHOTO][0].adjustments.exposure).toBe(2));
  thumbnailGate = null;
  release();
  await waitFor(() => expect(thumbnailCalls).toEqual(["1", "2"]));
  await waitFor(() => expect(useSnapshotStore.getState().thumbnails.a).toBe("blob:2"));
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:1");
});

it("renames from its own button without restoring the snapshot", async () => {
  kept[PHOTO] = [{ id: "a", name: "Look", createdAt: "2026-10-20T10:00:00.000Z", adjustments: recipe(3) }];
  render(<StudioSnapshots />);
  await screen.findByText("Look");
  fireEvent.click(screen.getByRole("button", { name: "Rename Look" }));
  const input = await screen.findByRole("textbox", { name: "Snapshot name" });
  // Clicks into the field neither restore nor end the rename.
  fireEvent.click(input);
  fireEvent.doubleClick(input);
  fireEvent.change(input, { target: { value: "Night" } });
  fireEvent.keyDown(input, { key: "Escape" });
  expect(kept[PHOTO][0].name).toBe("Look");
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
  expect(useEditorStore.getState().history).toHaveLength(1);
});

it("shows the lists in the order the changes were made", async () => {
  kept[PHOTO] = [{ id: "a", name: "Look", createdAt: "2026-10-20T10:00:00.000Z", adjustments: recipe(1) }];
  render(<StudioSnapshots />);
  await screen.findByText("Look");
  let release = () => {};
  renameGate = new Promise<void>((resolve) => (release = resolve));
  const store = useSnapshotStore.getState();
  const renamed = store.rename(PHOTO, "a", "Night");
  const updated = store.update(PHOTO, "a", recipe(2));
  await act(async () => {
    await Promise.resolve();
  });
  release();
  await act(async () => {
    await Promise.all([renamed, updated]);
  });
  const shown = useSnapshotStore.getState().snapshots[0];
  expect(shown.name).toBe("Night");
  expect(shown.adjustments.exposure).toBe(2);
});

it("answers each of two quick snapshots with the one it took", async () => {
  render(<StudioSnapshots />);
  await waitFor(() => expect(useSnapshotStore.getState().path).toBe(PHOTO));
  const store = useSnapshotStore.getState();
  const [first, second] = await act(() =>
    Promise.all([store.create(PHOTO, "One", recipe(1)), store.create(PHOTO, "Two", recipe(2))]),
  );
  expect([first?.name, second?.name]).toEqual(["One", "Two"]);
});

it("develops each thumbnail once when a photograph is shown again before they are done", async () => {
  kept[PHOTO] = [{ id: "a", name: "First", createdAt: "2026-10-20T10:00:00.000Z", adjustments: recipe(1) }];
  kept["/qa/other.dng"] = [];
  let release = () => {};
  thumbnailGate = new Promise<void>((resolve) => (release = resolve));
  const store = useSnapshotStore.getState();
  await act(() => store.load(PHOTO));
  await act(() => store.load("/qa/other.dng"));
  await act(() => store.load(PHOTO));
  thumbnailGate = null;
  release();
  await waitFor(() => expect(useSnapshotStore.getState().thumbnails.a).toBeTruthy());
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20));
  });
  // The first showing's result was dropped and released, the second kept.
  const created = vi.mocked(URL.createObjectURL).mock.results.map((r) => r.value as string);
  const shown = useSnapshotStore.getState().thumbnails.a;
  for (const url of created) if (url !== shown) expect(URL.revokeObjectURL).toHaveBeenCalledWith(url);
});
