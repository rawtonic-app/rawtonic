import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, type PointerEvent } from "react";
import { updateAdjustments } from "../../../hooks/useEditorActions";
import { useEditorStore } from "../../../store/useEditorStore";
import { INITIAL_LEVELS } from "../../../utils/adjustments";
import { levelsInputKey, pickedLevels } from "../../../utils/levels";
import {
  TAT_UNITS_PER_PIXEL,
  curveInputMean,
  dominantChannel,
  isLevelsPicker,
  isToneTarget,
  startTargetedAdjustment,
  targetedPatch,
  type TatStart,
} from "../../../utils/targetedAdjustment";
import { Invokes } from "../../ui/AppProperties";

/** Pointer travel (px) a press may move and still be a click. */
const CLICK_SLOP = 2;

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
 * receive, at the pressed point. The point curve target moves the channel
 * the curve panel shows and marks there the input level under the pointer
 * while it hovers; a click without a drag sets an anchor point.
 */
export default function TargetedAdjustmentOverlay({ rect }: { rect: Rect }) {
  const target = useEditorStore((s) => s.tatTarget);
  const drag = useRef<{
    startY: number;
    start: TatStart | null;
    frame: number | null;
    delta: number;
    /** Past CLICK_SLOP: a drag, no longer a click. */
    live: boolean;
    released: boolean;
  } | null>(null);

  // The hover sample: one at a time, the latest position follows it.
  const hover = useRef<{
    busy: boolean;
    next: { x: number; y: number } | null;
    /** The pointer is over the photo (a sample arriving after it left is
     * dropped). */
    inside: boolean;
    gone: boolean;
  }>({ busy: false, next: null, inside: false, gone: false });
  useEffect(() => {
    const h = hover.current;
    h.gone = false;
    return () => {
      h.gone = true;
      h.next = null;
      useEditorStore.getState().setEditor({ tatCurveMarker: null });
    };
  }, []);
  const sampleHover = async () => {
    const h = hover.current;
    if (h.busy || !h.next || drag.current) return;
    const { x, y } = h.next;
    h.next = null;
    const view = useEditorStore.getState();
    const { selectedImage, adjustments, tatTarget } = view;
    if (!selectedImage?.isReady || tatTarget !== "curve") return;
    h.busy = true;
    const path = selectedImage.path;
    const pixels = await invoke<[number, number, number][] | null>(
      Invokes.SampleCurveInput,
      { path, normalizedX: x, normalizedY: y, adjustments },
    ).catch(() => null);
    h.busy = false;
    const now = useEditorStore.getState();
    // A press since, or the tool or photo changed: the press marks it.
    if (
      !h.gone &&
      h.inside &&
      !drag.current &&
      now.tatTarget === "curve" &&
      now.selectedImage?.path === path
    ) {
      const marker = curveInputMean(
        "curve",
        now.adjustments,
        pixels,
        now.curveChannel,
      );
      if (marker !== now.tatCurveMarker)
        now.setEditor({ tatCurveMarker: marker });
    }
    if (h.next) void sampleHover();
  };

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
    const { selectedImage, adjustments, tatTarget, curveChannel, setEditor } =
      view;
    if (!selectedImage?.isReady || !tatTarget) return;
    hover.current.inside = true;
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
      live: false,
      released: false,
    };
    drag.current = record;
    const path = selectedImage.path;
    const point = { path, normalizedX, normalizedY };
    if (isLevelsPicker(tatTarget)) {
      // Levels' pickers: one click, one edit; the sample is what Levels
      // receives (rendered with Levels left out).
      drag.current = null;
      // The channel, recipe and native source clicked for: a tab switch, a
      // recipe edit (but to the levels) or a replaced source (the full
      // decode after the proxy) while sampling drops the sample.
      const channel = view.levelsChannel;
      const input = levelsInputKey(path, adjustments);
      const pixels = await invoke<[number, number, number][] | null>(
        Invokes.SampleCurveInput,
        { ...point, adjustments: { ...adjustments, levels: INITIAL_LEVELS } },
      ).catch(() => null);
      const now = useEditorStore.getState();
      if (
        now.selectedImage !== selectedImage ||
        now.tatTarget !== tatTarget ||
        now.levelsChannel !== channel ||
        levelsInputKey(path, now.adjustments) !== input
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
      curveChannel,
    );
    if (record.start)
      setEditor({
        tatFocus: dominantChannel(record.start),
        // Marked while the pointer is still over the photo.
        ...(tatTarget === "curve" && hover.current.inside && !hover.current.gone
          ? { tatCurveMarker: record.start.curveX ?? null }
          : {}),
      });
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
    if (!d) {
      if (useEditorStore.getState().tatTarget !== "curve") return;
      const bounds = e.currentTarget.getBoundingClientRect();
      hover.current.inside = true;
      hover.current.next = {
        x: (e.clientX - bounds.left) / bounds.width,
        y: (e.clientY - bounds.top) / bounds.height,
      };
      void sampleHover();
      return;
    }
    // A click that shakes a pixel or two stays a click (an anchor point).
    const moved = d.startY - e.clientY;
    if (Math.abs(moved) > CLICK_SLOP) d.live = true;
    if (!d.live) return;
    d.delta = moved * TAT_UNITS_PER_PIXEL * (e.shiftKey ? 0.25 : 1);
    if (d.frame === null) d.frame = requestAnimationFrame(apply);
  };

  const onPointerLeave = () => {
    hover.current.inside = false;
    hover.current.next = null;
    if (!drag.current)
      useEditorStore.getState().setEditor({ tatCurveMarker: null });
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
      onPointerLeave={onPointerLeave}
    />
  );
}
