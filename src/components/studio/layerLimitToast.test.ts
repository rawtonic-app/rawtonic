import "../../i18n";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useEditorStore } from "../../store/useEditorStore";
import { updateAdjustments } from "../../hooks/useEditorActions";
import { INITIAL_ADJUSTMENTS, INITIAL_MASK_CONTAINER, type Adjustments } from "../../utils/adjustments";
import { createSubMask } from "../../utils/maskUtils";
import { flushSliderDraft, registerSliderDraftRenderer } from "../../utils/sliderDraft";
import { Mask } from "../panel/right/Masks";
import { warnIfLayerLimit } from "./layerLimitToast";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
const { toastError } = vi.hoisted(() => ({ toastError: vi.fn() }));
vi.mock("react-toastify", () => ({ toast: { error: toastError } }));

const size = { width: 1000, height: 800 };
const layer = (id: string) => ({
  ...structuredClone(INITIAL_MASK_CONTAINER),
  id,
  subMasks: [createSubMask(Mask.Radial, size)],
});

afterEach(() => flushSliderDraft());

describe("layer limit during a slider drag", () => {
  it("counts the layers of the drag's draft", () => {
    const unregister = registerSliderDraftRenderer(() => true);
    useEditorStore.setState({
      selectedImage: { path: "/a.arw", isReady: true, ...size } as never,
      isSliderDragging: true,
      showOriginal: false,
      previewOverride: null,
      adjustments: { ...INITIAL_ADJUSTMENTS, masks: Array.from({ length: 31 }, (_, i) => layer(`m${i}`)) },
    });
    expect(warnIfLayerLimit()).toBe(false);
    updateAdjustments((prev: Adjustments) => ({ ...prev, masks: [...prev.masks, layer("new")] }));
    // Still in the draft, not yet in the store.
    expect(useEditorStore.getState().adjustments.masks).toHaveLength(31);
    expect(warnIfLayerLimit()).toBe(true);
    expect(toastError).toHaveBeenCalledTimes(1);
    unregister();
  });
});

describe("keepWithinLayerLimit", () => {
  it("keeps the state an update would push past the limit, and any other", async () => {
    const { keepWithinLayerLimit } = await import("../../utils/layerLimit");
    const at = (n: number) => ({ masks: Array.from({ length: n }, (_, i) => layer(`m${i}`)) });
    const full = at(32);
    expect(keepWithinLayerLimit(full, at(33))).toBe(full);
    const below = at(31);
    const next = at(32);
    expect(keepWithinLayerLimit(below, next)).toBe(next);
    // An older recipe beyond the limit may still lose layers or change others.
    const over = at(34);
    const fewer = at(33);
    expect(keepWithinLayerLimit(over, fewer)).toBe(fewer);
  });
});
