import "../../i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import StudioBrowser from "./StudioBrowser";
import {
  ownsPanelShortcut,
  focusWorkspaceControl,
} from "./workspaceInteractions";
import { type ImageFile } from "../ui/AppProperties";

beforeEach(() =>
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  ),
);
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});
describe("workspace focus ownership", () => {
  it("focuses clicked toolbar icons on WebKit before handling the next Tab", () => {
    const button = document.createElement("button");
    const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    button.append(icon);
    document.body.append(button);
    focusWorkspaceControl(icon);
    expect(document.activeElement).toBe(button);
    expect(ownsPanelShortcut(document.activeElement)).toBe(false);
    button.remove();
  });

  it("leaves Tab and Shift+Tab traversal on buttons, disclosure controls and the separator", () => {
    for (const tag of ["button", "summary", "input", "select", "textarea"])
      expect(ownsPanelShortcut(document.createElement(tag))).toBe(false);
    const separator = document.createElement("div");
    separator.tabIndex = 0;
    separator.setAttribute("role", "separator");
    expect(ownsPanelShortcut(separator)).toBe(false);
    const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    const button = document.createElement("button");
    button.append(icon);
    expect(ownsPanelShortcut(icon)).toBe(false);
  });

  it("gives Tab to the panels on the viewer, the photograph tiles and with nothing focused", () => {
    // Nothing focused: Lightroom hides the panels; WebKit reports the body.
    expect(ownsPanelShortcut(document.body)).toBe(true);
    expect(ownsPanelShortcut(null)).toBe(true);
    const viewer = document.createElement("div");
    viewer.tabIndex = 0;
    viewer.dataset.panelShortcut = "true";
    expect(ownsPanelShortcut(viewer)).toBe(true);
    // Only the marked element itself, not a control inside it.
    const inner = document.createElement("button");
    viewer.append(inner);
    expect(ownsPanelShortcut(inner)).toBe(false);
  });
});
describe("filmstrip selection reveal", () => {
  it("keeps a late selection visible on initial mount and after the folder count changes", () => {
    const files = Array.from({ length: 80 }, (_, n) => ({
      path: `/qa/photo-${n}.jpg`,
      rating: 0,
      is_raw: false,
      is_edited: false,
      tags: null,
      exif: null,
      modified: 0,
      is_virtual_copy: false,
      is_cloud_placeholder: false,
      group_id: null,
    })) satisfies ImageFile[];
    const props = {
      film: true,
      currentPath: files[75].path,
      onSelect: vi.fn(),
      onOpen: vi.fn(),
      onRate: vi.fn(),
      onContextMenu: vi.fn(),
      requestThumbnails: vi.fn(),
    };
    const { rerender } = render(<StudioBrowser {...props} files={files} />);
    const strip = screen.getByRole("listbox");
    expect(strip.scrollLeft).toBeGreaterThan(8000);
    rerender(<StudioBrowser {...props} files={files.slice(0, 79)} />);
    expect(strip.scrollLeft).toBeGreaterThan(8000);
  });
});

describe("grid scroll position", () => {
  it("keeps its place when photos leave or arrive, and starts at the top for a new listing", () => {
    const files = Array.from({ length: 400 }, (_, n) => ({
      path: `/qa/photo-${n}.jpg`,
      rating: 0,
      is_raw: false,
      is_edited: false,
      tags: null,
      exif: null,
      modified: 0,
      is_virtual_copy: false,
      is_cloud_placeholder: false,
      group_id: null,
    })) satisfies ImageFile[];
    const props = {
      onSelect: vi.fn(),
      onOpen: vi.fn(),
      onRate: vi.fn(),
      onContextMenu: vi.fn(),
      requestThumbnails: vi.fn(),
    };
    const { rerender } = render(<StudioBrowser {...props} files={files} listing="a" />);
    const grid = screen.getByRole("listbox");
    grid.scrollTop = 5000;
    // A rejected photo trashed while culling.
    rerender(<StudioBrowser {...props} files={files.slice(1)} listing="a" />);
    expect(grid.scrollTop).toBe(5000);
    rerender(<StudioBrowser {...props} files={files.slice(1)} listing="b" />);
    expect(grid.scrollTop).toBe(0);
  });
});

describe("grid keyboard reveal", () => {
  it("scrolls a photograph picked elsewhere into view, once per pick, and tells the keys its row length", async () => {
    const { useUIStore } = await import("../../store/useUIStore");
    const files = Array.from({ length: 400 }, (_, n) => ({
      path: `/qa/photo-${n}.jpg`,
      rating: 0,
      is_raw: false,
      is_edited: false,
      tags: null,
      exif: null,
      modified: 0,
      is_virtual_copy: false,
      is_cloud_placeholder: false,
      group_id: null,
    })) satisfies ImageFile[];
    const props = {
      onSelect: vi.fn(),
      onOpen: vi.fn(),
      onRate: vi.fn(),
      onContextMenu: vi.fn(),
      requestThumbnails: vi.fn(),
      listing: "a",
    };
    Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 500 });
    try {
      const { rerender, unmount } = render(<StudioBrowser {...props} files={files} currentPath={files[0].path} />);
      const grid = screen.getByRole("listbox");
      // 800 px wide at 190 px: 4 per row, rows of round(200 * 0.72) + 57 = 201 px.
      expect(useUIStore.getState().gridColumns).toBe(4);
      expect(grid.scrollTop).toBe(0);
      // Photo 40 is on row 10 (top 2010): scrolled so its row ends the view.
      rerender(<StudioBrowser {...props} files={files} currentPath={files[40].path} />);
      expect(grid.scrollTop).toBe(2010 + 201 - 500);
      // Its tile is mounted now.
      expect(grid.textContent).toContain("photo-40.jpg");
      // Back up to photo 4 (row 1): scrolled so its row starts the view.
      rerender(<StudioBrowser {...props} files={files} currentPath={files[4].path} />);
      expect(grid.scrollTop).toBe(201);
      // Scrolled away by hand: photos arriving do not pull it back.
      grid.scrollTop = 6000;
      rerender(<StudioBrowser {...props} files={[...files, { ...files[0], path: "/qa/new.jpg" }]} currentPath={files[4].path} />);
      expect(grid.scrollTop).toBe(6000);
      unmount();
      expect(useUIStore.getState().gridColumns).toBe(0);
    } finally {
      delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
    }
  });
});

describe("grid keyboard entry", () => {
  it("takes the focus to the active photograph when Tab enters a grid scrolled away from it", async () => {
    const { act, fireEvent } = await import("@testing-library/react");
    const { useLibraryStore } = await import("../../store/useLibraryStore");
    const files = Array.from({ length: 400 }, (_, n) => ({
      path: `/qa/photo-${n}.jpg`, rating: 0, is_raw: false, is_edited: false, tags: null, exif: null,
      modified: 0, is_virtual_copy: false, is_cloud_placeholder: false, group_id: null,
    })) satisfies ImageFile[];
    const props = { onSelect: vi.fn(), onOpen: vi.fn(), onRate: vi.fn(), onContextMenu: vi.fn(), requestThumbnails: vi.fn() };
    Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 500 });
    useLibraryStore.setState({ libraryActivePath: files[0].path });
    try {
      render(<StudioBrowser {...props} files={files} currentPath={files[0].path} />);
      const grid = screen.getByRole("listbox");
      grid.scrollTop = 201 * 13;
      fireEvent.scroll(grid);
      expect(grid.textContent).not.toContain("photo-0.jpg");
      // The fallback stop is the first tile in view; Tab focuses it.
      const stop = Array.from(grid.querySelectorAll<HTMLElement>("[role=option]")).find((o) => o.tabIndex === 0)!;
      expect(stop.getAttribute("aria-label")).not.toBe("photo-0.jpg");
      act(() => stop.focus());
      expect(grid.scrollTop).toBe(0);
      expect(document.activeElement?.getAttribute("aria-label")).toBe("photo-0.jpg");
    } finally {
      delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
      useLibraryStore.setState({ libraryActivePath: null });
    }
  });
});
