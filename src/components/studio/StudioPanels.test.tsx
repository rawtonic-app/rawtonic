import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import StudioWorkspace from "./StudioWorkspace";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { AppSettings, SelectedImage } from "../ui/AppProperties";
import { readModuleLayout } from "../../utils/panelLayout";
import { PEEK_HOLD_MS, PEEK_LEAVE_MS } from "./useEdgePeek";

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
  useSettingsStore.setState({
    appSettings: { keybinds: {} } as unknown as AppSettings,
    osPlatform: "macos",
  });
  useUIStore.setState((s) => ({
    activeView: "editor",
    studioToolLayout: "split-tools",
    uiVisibility: { ...s.uiVisibility, modules: undefined },
  }));
  useEditorStore.setState({
    selectedImage: {
      path: "/qa/current.dng",
      width: 6000,
      height: 4000,
      isRaw: true,
      isReady: true,
    } as SelectedImage,
  });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const noop = () => {};
const workspace = () =>
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
      onCopy={noop}
      onPaste={noop}
      onReset={noop}
      onZoom={noop}
      requestThumbnails={noop}
      settings={null}
      onSettings={noop}
    />,
  );

it("hides everything but the photograph and its tool bar with Shift+Tab, with strips on the edges", () => {
  const { container } = workspace();
  expect(container.querySelector(".studio-primary-panel")).not.toBeNull();
  expect(container.querySelector(".studio-secondary-panel")).not.toBeNull();
  expect(container.querySelector(".studio-filmstrip")).not.toBeNull();
  expect(container.querySelector(".studio-edge")).toBeNull();
  act(() => useUIStore.getState().togglePanels("all"));
  expect(container.querySelector(".studio-primary-panel")).toBeNull();
  expect(container.querySelector(".studio-secondary-panel")).toBeNull();
  expect(container.querySelector(".studio-filmstrip")).toBeNull();
  expect(container.querySelector(".studio-viewer-footer")).toBeNull();
  expect(container.querySelector(".studio-viewer-toolbar")).not.toBeNull();
  expect(container.querySelector(".studio-workspace.bars-hidden")).not.toBeNull();
  for (const name of [
    "Show the left panel",
    "Show the right panel",
    "Show the bars",
    "Show the bars and filmstrip",
  ])
    expect(screen.getByRole("button", { name })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Show the left panel" }).title).toBe(
    "Show the left panel (F7)",
  );
});

it("brings a hidden part back from its edge strip only", () => {
  const { container } = workspace();
  act(() => useUIStore.getState().togglePanels("all"));
  fireEvent.click(screen.getByRole("button", { name: "Show the right panel" }));
  expect(container.querySelector(".studio-primary-panel")).not.toBeNull();
  expect(container.querySelector(".studio-secondary-panel")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Show the bars and filmstrip" }));
  expect(container.querySelector(".studio-filmstrip")).not.toBeNull();
  expect(container.querySelector(".studio-viewer-footer")).not.toBeNull();
  expect(screen.queryByRole("button", { name: "Show the bars" })).toBeNull();
  expect(readModuleLayout(useUIStore.getState().uiVisibility.modules, "develop")).toEqual({
    left: false,
    right: true,
    filmstrip: true,
    bars: true,
  });
});

it("toggles both side panels from the tool bar button and the filmstrip from the status bar", () => {
  const { container } = workspace();
  const toggle = screen.getByRole("button", { name: "Toggle panels" });
  expect(toggle.title).toBe("Hide panels (Tab)");
  fireEvent.click(toggle);
  expect(container.querySelector(".studio-primary-panel")).toBeNull();
  expect(container.querySelector(".studio-secondary-panel")).toBeNull();
  expect(screen.getByRole("button", { name: "Toggle panels" }).title).toBe("Show panels (Tab)");
  fireEvent.click(screen.getByRole("button", { name: "Filmstrip" }));
  expect(container.querySelector(".studio-filmstrip")).toBeNull();
  // The status bar keeps its own button for the filmstrip; no bottom strip.
  expect(screen.queryByRole("button", { name: /^Show the bars/ })).toBeNull();
});

it("keeps the Library's layout apart from Develop's", () => {
  const { container } = workspace();
  act(() => useUIStore.getState().togglePanels("all"));
  act(() => useUIStore.setState({ activeView: "library" }));
  expect(container.querySelector(".studio-primary-panel.is-library")).not.toBeNull();
  expect(container.querySelector(".studio-library-footer")).not.toBeNull();
  expect(container.querySelector(".studio-edge")).toBeNull();
  act(() => useUIStore.getState().togglePanels("sides"));
  expect(container.querySelector(".studio-primary-panel")).toBeNull();
  // The Library has no panel on the right.
  expect(screen.queryByRole("button", { name: "Show the right panel" })).toBeNull();
  act(() => useUIStore.setState({ activeView: "editor" }));
  expect(container.querySelector(".studio-workspace.bars-hidden")).not.toBeNull();
});

describe("hidden side panels on the window edge (Auto Hide & Show)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());
  const strip = (side: "left" | "right") =>
    screen.getByRole("button", { name: `Show the ${side} panel` }).parentElement as HTMLElement;
  const center = (container: HTMLElement) => container.querySelector(".studio-center") as Element;

  it("shows the panel over the photograph as soon as the pointer stays on the edge, and hides it when the pointer leaves", () => {
    const { container } = workspace();
    act(() => useUIStore.getState().togglePanels("right"));
    expect(container.querySelector(".studio-primary-panel")).toBeNull();
    fireEvent.pointerEnter(strip("right"), { buttons: 0 });
    fireEvent.pointerMove(strip("right"), { buttons: 0 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS - 5));
    expect(container.querySelector(".studio-primary-panel")).toBeNull();
    act(() => vi.advanceTimersByTime(5));
    const panel = container.querySelector(".studio-primary-panel.is-peek[data-peek='right']") as Element;
    expect(panel).not.toBeNull();
    // Shown for the moment only: the stored layout still has it hidden.
    expect(readModuleLayout(useUIStore.getState().uiVisibility.modules, "develop").right).toBe(false);
    // Moving from the strip into the panel keeps it.
    fireEvent.pointerLeave(strip("right"), { relatedTarget: panel });
    expect(container.querySelector(".studio-primary-panel.is-peek")).not.toBeNull();
    // A slip over the border and back keeps it; leaving for good hides it.
    fireEvent.pointerLeave(panel, { buttons: 0, relatedTarget: center(container) });
    act(() => vi.advanceTimersByTime(PEEK_LEAVE_MS - 50));
    // (React derives enter/leave from the `pointerout` of the element left.)
    fireEvent.pointerOut(center(container), { buttons: 0, relatedTarget: panel });
    act(() => vi.advanceTimersByTime(PEEK_LEAVE_MS * 2));
    expect(container.querySelector(".studio-primary-panel.is-peek")).not.toBeNull();
    fireEvent.pointerLeave(panel, { buttons: 0, relatedTarget: center(container) });
    act(() => vi.advanceTimersByTime(PEEK_LEAVE_MS));
    expect(container.querySelector(".studio-primary-panel")).toBeNull();
  });

  it("does not open when the pointer only crosses the edge on its way out of the window", () => {
    const { container } = workspace();
    act(() => useUIStore.getState().togglePanels("sides"));
    fireEvent.pointerEnter(strip("left"), { buttons: 0 });
    fireEvent.pointerMove(strip("left"), { buttons: 0 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS / 2));
    fireEvent.pointerLeave(strip("left"), { relatedTarget: null });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS * 10));
    expect(container.querySelector(".studio-secondary-panel")).toBeNull();
  });

  it("opens on a slow approach without waiting for the pointer to rest", () => {
    const { container } = workspace();
    act(() => useUIStore.getState().togglePanels("sides"));
    // Moving on across the strip does not restart the wait.
    fireEvent.pointerEnter(strip("left"), { buttons: 0, clientX: 11 });
    for (const x of [10, 9, 8]) {
      act(() => vi.advanceTimersByTime(Math.ceil(PEEK_HOLD_MS / 3)));
      fireEvent.pointerMove(strip("left"), { buttons: 0, clientX: x });
    }
    expect(container.querySelector(".studio-secondary-panel.is-peek[data-peek='left']")).not.toBeNull();
  });

  it("does not open while a button is held (slider, brush or crop drag reaching the edge)", () => {
    const { container } = workspace();
    act(() => useUIStore.getState().togglePanels("sides"));
    fireEvent.pointerEnter(strip("left"), { buttons: 1 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS * 10));
    expect(container.querySelector(".studio-secondary-panel")).toBeNull();
  });

  it("keeps a peeking panel during a drag that leaves it and decides on release", () => {
    const { container } = workspace();
    act(() => useUIStore.getState().togglePanels("sides"));
    fireEvent.pointerEnter(strip("left"), { buttons: 0 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS));
    const panel = container.querySelector(".studio-secondary-panel.is-peek[data-peek='left']") as Element;
    expect(panel).not.toBeNull();
    fireEvent.pointerLeave(panel, { buttons: 1, relatedTarget: center(container) });
    expect(container.querySelector(".studio-secondary-panel")).not.toBeNull();
    document.elementFromPoint = vi.fn(() => center(container));
    act(() => {
      window.dispatchEvent(new MouseEvent("pointerup", { clientX: 600, clientY: 300 }));
    });
    act(() => vi.advanceTimersByTime(PEEK_LEAVE_MS));
    expect(container.querySelector(".studio-secondary-panel")).toBeNull();
  });

  it("keeps the panel when its strip is clicked", () => {
    const { container } = workspace();
    act(() => useUIStore.getState().togglePanels("right"));
    fireEvent.pointerEnter(strip("right"), { buttons: 0 });
    act(() => vi.advanceTimersByTime(PEEK_HOLD_MS));
    fireEvent.click(screen.getByRole("button", { name: "Show the right panel" }));
    const panel = container.querySelector(".studio-primary-panel");
    expect(panel).not.toBeNull();
    expect(panel?.classList.contains("is-peek")).toBe(false);
    expect(readModuleLayout(useUIStore.getState().uiVisibility.modules, "develop").right).toBe(true);
  });
});
