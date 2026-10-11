import { afterEach, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import { useBrushMemory } from "./useBrushMemory";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { AppSettings } from "../ui/AppProperties";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
afterEach(() => vi.useRealTimers());

it("restores the remembered brush and saves changes once a drag ends", () => {
  vi.useFakeTimers();
  useEditorStore.setState({ brushSettings: { size: 100, feather: 50, tool: "brush" as never } });
  useSettingsStore.setState({
    appSettings: { brushDefaults: { size: 240, hardness: 20, opacity: 60, flow: 30 } } as unknown as AppSettings,
  });
  const hook = renderHook(() => useBrushMemory());
  expect(useEditorStore.getState().brushSettings).toMatchObject({ size: 240, hardness: 20, opacity: 60, flow: 30 });
  const brush = useEditorStore.getState().brushSettings!;
  useEditorStore.setState({ brushSettings: { ...brush, size: 300 } });
  useEditorStore.setState({ brushSettings: { ...brush, size: 320 } });
  expect(useSettingsStore.getState().appSettings?.brushDefaults?.size).toBe(240);
  vi.advanceTimersByTime(700);
  expect(useSettingsStore.getState().appSettings?.brushDefaults).toEqual({
    size: 320,
    hardness: 20,
    opacity: 60,
    flow: 30,
  });
  hook.unmount();
});
