import { act, cleanup, render } from "@testing-library/react";
import "../../i18n";
import { createRef } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import Editor, { type TransformWrapperHandle } from "./Editor";
import { useEditorStore } from "../../store/useEditorStore";

// The renderer is replaced; the test plays the canvas reporting a pick.
const canvas = vi.hoisted(() => ({ props: null as null | { onWbPicked?(keep?: boolean): void } }));
vi.mock("./editor/ImageCanvas", () => ({
  default: (props: { onWbPicked?(keep?: boolean): void }) => {
    canvas.props = props;
    return null;
  },
}));
vi.mock("../../hooks/useImageRenderSize", () => ({
  useImageRenderSize: () => ({
    width: 400,
    height: 300,
    scale: 0.1,
    offsetX: 0,
    offsetY: 0,
    containerWidth: 400,
    containerHeight: 300,
    margin: 0,
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

beforeEach(() => {
  vi.stubGlobal("requestAnimationFrame", () => 1);
  vi.stubGlobal("cancelAnimationFrame", () => {});
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  useEditorStore.setState({
    isWbPickerActive: true,
    colorPickerTarget: null,
    selectedImage: { path: "/a.raw", width: 4000, height: 3000, isReady: true } as never,
    displaySize: { width: 400, height: 300 },
  });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("ends the white balance picker after one pick unless Shift kept it on", () => {
  // UXR1 D5 (coordinator decision 2026-10-31): one correction is the common
  // case, as in Lightroom; Shift-click keeps picking, Esc still ends it.
  render(<Editor onContextMenu={() => {}} transformWrapperRef={createRef<TransformWrapperHandle>()} />);
  act(() => canvas.props!.onWbPicked!(true));
  expect(useEditorStore.getState().isWbPickerActive).toBe(true);
  act(() => canvas.props!.onWbPicked!(false));
  expect(useEditorStore.getState().isWbPickerActive).toBe(false);
});
