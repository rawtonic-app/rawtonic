import { describe, expect, it, vi } from "vitest";
import { createRef } from "react";
import { renderHook } from "@testing-library/react";
import { libraryHeaderFits, useLibraryHeaderFits } from "./libraryHeaderLayout";

describe("Library header breakpoint", () => {
  it.each([1, 1.12, 1.25])("keeps two rows below 1440 logical pixels at scale %s", (scale) => {
    expect(libraryHeaderFits(1280, scale)).toBe(false);
    expect(libraryHeaderFits(1440 * scale - 1, scale)).toBe(false);
    expect(libraryHeaderFits(1440 * scale, scale)).toBe(true);
  });
  it("fits the wide window at 100 and 112 percent, keeps room at 125 percent", () => {
    expect(libraryHeaderFits(1728, 1)).toBe(true);
    expect(libraryHeaderFits(1728, 1.12)).toBe(true);
    expect(libraryHeaderFits(1728, 1.25)).toBe(false);
  });
});


it("falls back if translated or active controls exceed the available header slot", () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  Object.defineProperty(window, "innerWidth", { value: 1728, configurable: true });
  const toolbar = document.createElement("div");
  const control = document.createElement("button");
  toolbar.append(control);
  const bounds = (right: number) => ({ right } as DOMRect);
  toolbar.getBoundingClientRect = () => bounds(800);
  control.getBoundingClientRect = () => bounds(900);
  const ref = createRef<HTMLDivElement>();
  ref.current = toolbar;
  const hook = renderHook(({ content }) => useLibraryHeaderFits(1, ref, content, true), {
    initialProps: { content: "active German filters" },
  });
  expect(hook.result.current).toBe(false);
  control.getBoundingClientRect = () => bounds(790);
  hook.rerender({ content: "cleared filters" });
  expect(hook.result.current).toBe(true);
  hook.unmount();
  vi.unstubAllGlobals();
});
