import { useEffect, useRef, useState } from "react";
import i18n from "i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { Panel } from "../ui/AppProperties";
import type { Adjustments } from "../../utils/adjustments";
import { swallowNextClick } from "../../utils/swallowClick";

type QuickAdjustLabel =
  | "studio.develop.sliders.exposure"
  | "studio.develop.sliders.contrast"
  | "studio.develop.sliders.brightness"
  | "adjustments.color.saturation"
  | "studio.develop.sliders.highlights"
  | "studio.develop.sliders.shadows"
  | "studio.develop.sliders.whites"
  | "studio.develop.sliders.blacks"
  | "adjustments.details.clarity";

/** Capture One's Quick Adjust: hold a key and scroll or drag over the photo
 * to change one control (keys from Capture One 16's defaults). */
export const QUICK_ADJUST: Record<
  string,
  {
    key: keyof Adjustments & string;
    /** i18n key of the shown name. */
    label: QuickAdjustLabel;
    min: number;
    max: number;
    wheel: number;
    pixel: number;
  }
> = {
  q: { key: "exposure", label: "studio.develop.sliders.exposure", min: -5, max: 5, wheel: 0.05, pixel: 0.005 },
  w: { key: "contrast", label: "studio.develop.sliders.contrast", min: -100, max: 100, wheel: 1, pixel: 0.25 },
  e: { key: "brightness", label: "studio.develop.sliders.brightness", min: -5, max: 5, wheel: 0.05, pixel: 0.005 },
  r: { key: "saturation", label: "adjustments.color.saturation", min: -100, max: 100, wheel: 1, pixel: 0.25 },
  a: { key: "highlights", label: "studio.develop.sliders.highlights", min: -100, max: 100, wheel: 1, pixel: 0.25 },
  s: { key: "shadows", label: "studio.develop.sliders.shadows", min: -100, max: 100, wheel: 1, pixel: 0.25 },
  d: { key: "whites", label: "studio.develop.sliders.whites", min: -100, max: 100, wheel: 1, pixel: 0.25 },
  f: { key: "blacks", label: "studio.develop.sliders.blacks", min: -100, max: 100, wheel: 1, pixel: 0.25 },
  z: { key: "clarity", label: "adjustments.details.clarity", min: -100, max: 100, wheel: 1, pixel: 0.25 },
};

/** Marks the key event replayed after a plain tap, so it reaches the key's
 * usual action (a tool, full screen, …). */
const REPLAYED = Symbol("quick-adjust-replay");

export interface QuickAdjustReadout {
  label: string;
  value: number;
  x: number;
  y: number;
}

function inViewer(target: EventTarget | null) {
  return target instanceof Element && Boolean(target.closest(".studio-viewer"));
}

/** Active in Develop. Returns the readout to show beside the pointer. */
export function useQuickAdjust(active: boolean) {
  const { setAdjustments } = useEditorActions();
  const setRef = useRef(setAdjustments);
  setRef.current = setAdjustments;
  const [readout, setReadout] = useState<QuickAdjustReadout | null>(null);

  useEffect(() => {
    if (!active) return;
    let held: { letter: string; used: boolean; event: KeyboardEvent } | null = null;
    let pointer = { x: 0, y: 0 };
    let drag: { x: number } | null = null;

    const target = () => {
      const editor = useEditorStore.getState();
      const layer =
        useUIStore.getState().activePanel === Panel.Masks
          ? editor.adjustments.masks.find((m) => m.id === editor.activeMaskContainerId)
          : undefined;
      return { editor, layer };
    };
    const change = (amount: number) => {
      if (!held) return;
      const spec = QUICK_ADJUST[held.letter];
      const { editor, layer } = target();
      if (!editor.selectedImage?.isReady) return;
      const values = (layer?.adjustments ?? editor.adjustments) as unknown as Record<string, number>;
      const current = Number(values[spec.key] ?? 0);
      const digits = spec.wheel < 1 ? 2 : 0;
      const next = Number(Math.min(spec.max, Math.max(spec.min, current + amount)).toFixed(digits));
      held.used = true;
      if (!editor.isSliderDragging) editor.setEditor({ isSliderDragging: true });
      setRef.current((prev) =>
        layer
          ? {
              ...prev,
              masks: prev.masks.map((m) =>
                m.id === layer.id ? { ...m, adjustments: { ...m.adjustments, [spec.key]: next } } : m,
              ),
            }
          : { ...prev, [spec.key]: next },
      );
      setReadout({ label: i18n.t(spec.label), value: next, x: pointer.x, y: pointer.y });
    };
    const finish = () => {
      if (held?.used) useEditorStore.getState().setEditor({ isSliderDragging: false });
      held = null;
      drag = null;
      setReadout(null);
    };

    const onKeyDown = (e: KeyboardEvent) => {
      if ((e as KeyboardEvent & { [REPLAYED]?: boolean })[REPLAYED]) return;
      const letter = e.key.toLowerCase();
      if (held && letter === held.letter) {
        // Auto-repeat while held.
        e.preventDefault();
        e.stopImmediatePropagation();
        return;
      }
      if (
        held ||
        !QUICK_ADJUST[letter] ||
        e.metaKey ||
        e.ctrlKey ||
        e.altKey ||
        e.repeat ||
        document.querySelector('dialog[open],[aria-modal="true"]') ||
        (e.target instanceof HTMLElement && e.target.closest('input,textarea,select,[contenteditable="true"]')) ||
        !useEditorStore.getState().selectedImage?.isReady
      )
        return;
      // Defer the key: a tap still does what the key did before.
      e.preventDefault();
      e.stopImmediatePropagation();
      held = { letter, used: false, event: e };
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (!held || e.key.toLowerCase() !== held.letter) return;
      const { used, event } = held;
      finish();
      if (!used) {
        const replay = new KeyboardEvent("keydown", {
          key: event.key,
          code: event.code,
          shiftKey: event.shiftKey,
          bubbles: true,
          cancelable: true,
        });
        (replay as KeyboardEvent & { [REPLAYED]?: boolean })[REPLAYED] = true;
        (event.target instanceof EventTarget ? event.target : window).dispatchEvent(replay);
      }
    };
    const onBlur = () => finish();
    const onWheel = (e: WheelEvent) => {
      if (!held || !inViewer(e.target)) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      pointer = { x: e.clientX, y: e.clientY };
      const spec = QUICK_ADJUST[held.letter];
      // Scrolling up raises the value, as Capture One's wheel.
      change(-Math.sign(e.deltaY || e.deltaX) * spec.wheel);
    };
    const onPointerDown = (e: PointerEvent) => {
      if (!held || e.button !== 0 || !inViewer(e.target)) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      swallowNextClick();
      drag = { x: e.clientX };
      pointer = { x: e.clientX, y: e.clientY };
    };
    const onPointerMove = (e: PointerEvent) => {
      pointer = { x: e.clientX, y: e.clientY };
      if (!held || !drag) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      const dx = e.clientX - drag.x;
      if (dx === 0) return;
      drag.x = e.clientX;
      change(dx * QUICK_ADJUST[held.letter].pixel);
    };
    const onPointerUp = (e: PointerEvent) => {
      if (!drag) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      drag = null;
    };

    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("keyup", onKeyUp, true);
    window.addEventListener("blur", onBlur);
    window.addEventListener("wheel", onWheel, { capture: true, passive: false });
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("pointermove", onPointerMove, true);
    window.addEventListener("pointerup", onPointerUp, true);
    return () => {
      finish();
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("keyup", onKeyUp, true);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("wheel", onWheel, true);
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("pointermove", onPointerMove, true);
      window.removeEventListener("pointerup", onPointerUp, true);
    };
  }, [active]);

  return readout;
}
