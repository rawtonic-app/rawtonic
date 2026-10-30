import { invoke } from "@tauri-apps/api/core";
import { useRef, type PointerEvent } from "react";
import { updateAdjustments } from "../../../hooks/useEditorActions";
import { useEditorStore } from "../../../store/useEditorStore";
import { INITIAL_LEVELS } from "../../../utils/adjustments";
import { pickedLevels } from "../../../utils/levels";
import {
  TAT_UNITS_PER_PIXEL,
  dominantChannel,
  isLevelsPicker,
  isToneTarget,
  startTargetedAdjustment,
  targetedPatch,
  type TatStart,
} from "../../../utils/targetedAdjustment";
import { Invokes } from "../../ui/AppProperties";

interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * The Targeted Adjustment Tool's surface over the displayed photo: press
 * on a colour or tone and drag up or down (Shift: finer). Colour targets
 * read the photo's colour before adjustments, the curves the colour they
 * receive, at the pressed point.
 */
export default function TargetedAdjustmentOverlay({ rect }: { rect: Rect }) {
  const target = useEditorStore((s) => s.tatTarget);
  const drag = useRef<{
    startY: number;
    start: TatStart | null;
    frame: number | null;
    delta: number;
    released: boolean;
  } | null>(null);

  const apply = () => {
    const d = drag.current;
    if (!d) return;
    d.frame = null;
    if (d.start) {
      const patch = targetedPatch(d.start, d.delta);
      updateAdjustments((prev) => ({ ...prev, ...patch }));
    }
  };

  const onPointerDown = async (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const view = useEditorStore.getState();
    const { selectedImage, adjustments, tatTarget, setEditor } = view;
    if (!selectedImage?.isReady || !tatTarget) return;
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // A pointer the browser does not track (synthetic input): no capture.
    }
    const bounds = e.currentTarget.getBoundingClientRect();
    const normalizedX = (e.clientX - bounds.left) / bounds.width;
    const normalizedY = (e.clientY - bounds.top) / bounds.height;
    const record = {
      startY: e.clientY,
      start: null as TatStart | null,
      frame: null,
      delta: 0,
      released: false,
    };
    drag.current = record;
    const path = selectedImage.path;
    const point = { path, normalizedX, normalizedY };
    if (isLevelsPicker(tatTarget)) {
      // Levels' pickers: one click, one edit; the sample is what Levels
      // receives (rendered with Levels left out).
      drag.current = null;
      // The channel clicked for: a tab switch while sampling cancels.
      const channel = view.levelsChannel;
      const pixels = await invoke<[number, number, number][] | null>(
        Invokes.SampleCurveInput,
        { ...point, adjustments: { ...adjustments, levels: INITIAL_LEVELS } },
      ).catch(() => null);
      const now = useEditorStore.getState();
      if (
        now.selectedImage?.path !== path ||
        now.tatTarget !== tatTarget ||
        now.levelsChannel !== channel
      )
        return;
      const levels = pickedLevels(
        now.adjustments.levels,
        channel,
        tatTarget === "levelsBlack" ? "black" : "white",
        pixels,
      );
      if (levels) updateAdjustments((prev) => ({ ...prev, levels }));
      setEditor({ tatTarget: null });
      return;
    }
    // Tone targets read the colour the curves receive, rendered from the
    // recipe without them and what follows (layer curves, LUTs, overlays).
    const [linear, curveInput] = await Promise.all([
      isToneTarget(tatTarget)
        ? Promise.resolve(null)
        : invoke<[number, number, number] | null>(
            Invokes.SamplePreviewWhiteBalance,
            point,
          ).catch(() => null),
      isToneTarget(tatTarget)
        ? invoke<[number, number, number][] | null>(Invokes.SampleCurveInput, {
            ...point,
            adjustments,
          }).catch(() => null)
        : Promise.resolve(null),
    ]);
    if (useEditorStore.getState().selectedImage?.path !== path) return;
    // Drag mode (interactive previews) only after sampling: the sample
    // renders from the preview base the shown frame was built from.
    if (!record.released) setEditor({ isSliderDragging: true });
    record.start = startTargetedAdjustment(
      tatTarget,
      adjustments,
      linear,
      curveInput,
    );
    if (record.start) setEditor({ tatFocus: dominantChannel(record.start) });
    // A quick flick can end before the sample arrives: apply where it ended.
    if (record.released && record.start) {
      updateAdjustments((prev) => ({
        ...prev,
        ...targetedPatch(record.start!, record.delta),
      }));
    }
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    d.delta =
      (d.startY - e.clientY) * TAT_UNITS_PER_PIXEL * (e.shiftKey ? 0.25 : 1);
    if (d.frame === null) d.frame = requestAnimationFrame(apply);
  };

  const end = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    if (d.frame !== null) cancelAnimationFrame(d.frame);
    apply();
    d.released = true;
    drag.current = null;
    if (e.currentTarget.hasPointerCapture(e.pointerId))
      e.currentTarget.releasePointerCapture(e.pointerId);
    useEditorStore.getState().setEditor({ isSliderDragging: false });
  };

  return (
    <div
      aria-label="Targeted adjustment: drag up or down on the photo"
      data-testid="targeted-adjustment"
      style={{
        cursor: target && isLevelsPicker(target) ? "crosshair" : "ns-resize",
        position: "absolute",
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        zIndex: 6,
        touchAction: "none",
      }}
      onPointerDown={(e) => void onPointerDown(e)}
      onPointerMove={onPointerMove}
      onPointerUp={end}
      onPointerCancel={end}
    />
  );
}
