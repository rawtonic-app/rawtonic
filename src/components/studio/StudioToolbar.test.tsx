import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import StudioWorkspace from "./StudioWorkspace";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import type { SelectedImage } from "../ui/AppProperties";

vi.mock("../panel/Editor", () => ({ default: () => null }));
vi.mock("./StudioDevelop", () => ({
  default: () => null,
  ToolGroup: () => null,
}));
vi.mock("./StudioNavigation", () => ({ default: () => null }));

beforeEach(() => {
  HTMLDialogElement.prototype.close = vi.fn();
  HTMLDialogElement.prototype.showModal = vi.fn();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  useUIStore.setState({ activeView: "editor" });
  useEditorStore.setState({
    selectedImage: {
      path: "/qa/current.dng",
      width: 6000,
      height: 4000,
      isRaw: true,
      isReady: true,
    } as SelectedImage,
  });
  useLibraryStore.setState({ multiSelectedPaths: ["/qa/other.dng"] });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("sends the open photo to Paste and Reset instead of the click event or old Library selection", () => {
  const paste = vi.fn(),
    reset = vi.fn(),
    copy = vi.fn(),
    noop = vi.fn();
  render(
    <StudioWorkspace
      files={[]}
      flagCounts={{ all: 0, picked: 0, rejected: 0, unflagged: 0 }}
      transformWrapperRef={{ current: null }}
      onSelect={noop}
      onFilmSelect={noop}
      onOpen={noop}
      onNavigate={noop}
      onBack={noop}
      onRate={noop}
      onContextMenu={noop}
      onEditorContextMenu={noop}
      onCopy={copy}
      onPaste={paste}
      onReset={reset}
      onZoom={noop}
      requestThumbnails={noop}
      settings={null}
      onSettings={noop}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Reset all adjustments" }));
  fireEvent.click(screen.getByRole("button", { name: "Paste adjustments" }));
  fireEvent.click(screen.getByRole("button", { name: "Copy adjustments" }));
  expect(reset).toHaveBeenCalledExactlyOnceWith(["/qa/current.dng"]);
  expect(paste).toHaveBeenCalledExactlyOnceWith(["/qa/current.dng"]);
  expect(copy).toHaveBeenCalledExactlyOnceWith();
  act(() => useEditorStore.setState((state) => ({ selectedImage: { ...state.selectedImage!, isReady: false } })));
  fireEvent.click(screen.getByRole("button", { name: "Reset all adjustments" }));
  fireEvent.click(screen.getByRole("button", { name: "Paste adjustments" }));
  fireEvent.click(screen.getByRole("button", { name: "Copy adjustments" }));
  expect(reset).toHaveBeenCalledTimes(1);
  expect(paste).toHaveBeenCalledTimes(1);
  expect(copy).toHaveBeenCalledTimes(1);
});
