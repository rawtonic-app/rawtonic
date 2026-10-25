import "../../i18n";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { useStudioTools } from "./useStudioTools";
import { useEditorStore } from "../../store/useEditorStore";
import { INITIAL_ADJUSTMENTS, newNegative } from "../../utils/adjustments";
import { setFilmBasePicking, startFilmBasePick, withPickedFilmBase } from "../../utils/negative";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
vi.mock("react-toastify", () => ({ toast: { error: vi.fn() } }));
vi.mock("../../hooks/useEditorActions", () => ({ useEditorActions: () => ({ setAdjustments: vi.fn() }) }));

beforeEach(() => {
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    selectedImage: { path: "/a.nef", isReady: true, width: 1000, height: 800 } as never,
    adjustments: { ...structuredClone(INITIAL_ADJUSTMENTS), negative: newNegative([0.7, 0.4, 0.2], [0.1, 0.05, 0.02]) },
  });
});
afterEach(cleanup);

it("hands the photo to another canvas tool: the film base picker and its pending sample end", () => {
  let tools: ReturnType<typeof useStudioTools> | undefined;
  const Probe = () => {
    tools = useStudioTools();
    return null;
  };
  render(<Probe />);
  for (const tool of ["spot", "brush", "wb", "crop"] as const) {
    setFilmBasePicking(true);
    const recipe = useEditorStore.getState().adjustments;
    const pick = startFilmBasePick("/a.nef", recipe);
    tools!.selectTool(tool);
    expect(useEditorStore.getState().filmBasePicking, tool).toBe(false);
    expect(withPickedFilmBase(recipe, pick, [0.8, 0.5, 0.3]), tool).toBe(recipe);
  }
});
