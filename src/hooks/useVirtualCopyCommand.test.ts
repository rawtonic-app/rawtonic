import "../i18n";
import { afterEach, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { useLibraryStore } from "../store/useLibraryStore";
import { useEditorStore } from "../store/useEditorStore";
import { useUIStore } from "../store/useUIStore";
import { runAppCommand } from "../utils/appCommands";
import { buildComboMap } from "../utils/keyboardUtils";
import { useVirtualCopyCommand } from "./useVirtualCopyCommand";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
vi.mock("../activity/notify", () => ({ notify: { error: vi.fn() } }));

afterEach(() => vi.mocked(invoke).mockClear());

// UXR3: a virtual copy took three actions (right-click, Duplicate photo,
// Virtual copy); Lightroom makes one with ⌘' and Capture One with a key too.
it("makes a virtual copy of the one selected or open photo with ⌘'", async () => {
  expect(buildComboMap(undefined).get("ctrl+Quote")).toContain("virtual_copy");
  const refresh = vi.fn(async () => {});
  const hook = renderHook(() => useVirtualCopyCommand(refresh));
  useUIStore.setState({ activeView: "library" });
  useLibraryStore.setState({ multiSelectedPaths: ["/job/a.CR3", "/job/b.CR3"], activeAlbumId: null });
  expect(runAppCommand("virtual_copy")).toBe(false); // two photos
  useLibraryStore.setState({ multiSelectedPaths: ["/job/a.CR3"] });
  expect(runAppCommand("virtual_copy")).toBe(true);
  await vi.waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
  expect(invoke).toHaveBeenCalledWith("create_virtual_copy", { sourceVirtualPath: "/job/a.CR3", targetAlbumId: null });
  // In Develop: the open photo.
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({ selectedImage: { path: "/job/c.CR3", isReady: true } as never });
  expect(runAppCommand("virtual_copy")).toBe(true);
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("create_virtual_copy", { sourceVirtualPath: "/job/c.CR3", targetAlbumId: null }),
  );
  hook.unmount();
  expect(runAppCommand("virtual_copy")).toBe(false);
});
