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
