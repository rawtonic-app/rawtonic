import {
  memo,
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { selectOnFocus } from "../../utils/selectOnFocus";
import { MIN_SEND_INTERVAL_MS } from "../../hooks/useDragSend";

export interface AdjustmentControlProps {
  label: string;
  /** Accessible name when the visible label repeats in another tool. */
  ariaLabel?: string;
  value: number;
  min?: number;
  max?: number;
  step?: number;
  defaultValue?: number;
  unit?: string;
  track?: "temperature" | "tint" | "hue";
  /** Non-linear track: maps a value to its slider position and back
   * (e.g. Kelvin in mireds). Keys and the text field stay in value units. */
  scale?: {
    toTrack(value: number): number;
    fromTrack(position: number): number;
  };
  disabled?: boolean;
  onChange(value: number): void;
  onInteraction?(active: boolean): void;
}

/** The thumb's width in the stylesheet (`.adjustment-track` thumb, 9 px). */
const THUMB = 9;
/** A press this close to the thumb's centre grabs it without a jump. */
const GRAB = 8;

/**
 * How much of the track's own movement a drag makes per pixel when the
 * pointer is `distance` px above or below the track: all of it on and near
 * the track, then less the farther away (half at 48 px, a quarter at 96,
 * a tenth from 240), as on a scrubbing bar. A small hand movement then
 * makes a small change, without a modifier key (notes:
 * reference/slider-precision-2026-11-01.md).
 */
export function dragPrecision(distance: number) {
  const near = 24;
  return distance <= near ? 1 : Math.max(0.1, near / distance);
}

export function boundedValue(
  value: number,
  min: number,
  max: number,
  step: number,
) {
  const digits = Math.min(6, Math.max(0, -Math.floor(Math.log10(step))));
  return Number(Math.max(min, Math.min(max, value)).toFixed(digits));
}

/** The value as the field shows it: at most the fine (Alt) step's
 * precision, so a stored 1.6009 reads 1.6 and fits the field. */
export function shownValue(value: number, step: number) {
  const digits = Math.min(6, Math.max(0, -Math.floor(Math.log10(step / 10))));
  return String(Number(value.toFixed(digits)));
}

/** Independent desktop control: local feedback, engine updates as input
 * arrives, at most one per 8 ms. */
export default memo(function AdjustmentControl({
  label,
  ariaLabel,
  value,
  min = -100,
  max = 100,
  step = 1,
  defaultValue = 0,
  unit = "",
  track,
  scale,
  disabled = false,
  onChange,
  onInteraction,
}: AdjustmentControlProps) {
  const { t } = useTranslation();
  const id = useId();
  const [local, setLocal] = useState(value);
  const [text, setText] = useState(shownValue(value, step));
  // A slider or label drag in progress: the row shows its pressed state.
  const [dragging, setDragging] = useState(false);
  const active = useRef(false);
  const frame = useRef<number | null>(null);
  const pending = useRef(value);
  const lastSent = useRef(value);
  const sentAt = useRef(-Infinity);
  const canceled = useRef(false);
  const callback = useRef(onChange);
  callback.current = onChange;
  const interactionCallback = useRef(onInteraction);
  interactionCallback.current = onInteraction;
  const scrub = useRef<{ x: number; value: number } | null>(null);
  // A drag on the track: from the thumb without a jump, a pixel worth the
  // track's own movement (a tenth with Option, less away from the track).
  const fine = useRef<{
    x: number;
    centreY: number;
    value: number;
    perPixel: number;
  } | null>(null);
  // Slider positions: value units, or the scale's with 1000 steps.
  const trackMin = scale ? scale.toTrack(min) : min;
  const trackMax = scale ? scale.toTrack(max) : max;
  const trackStep = scale ? (trackMax - trackMin) / 1000 : step;
  const toTrack = (v: number) => (scale ? scale.toTrack(v) : v);
  const fromTrack = (p: number) => (scale ? scale.fromTrack(p) : p);
  useEffect(() => {
    if (!active.current) {
      setLocal(value);
      setText(shownValue(value, step));
      pending.current = value;
      lastSent.current = value;
    }
  }, [value, step]);
  useEffect(
    () => () => {
      if (frame.current !== null) cancelAnimationFrame(frame.current);
      if (active.current) {
        if (lastSent.current !== pending.current)
          callback.current(pending.current);
        interactionCallback.current?.(false);
      }
    },
    [],
  );
  const flush = () => {
    if (frame.current !== null) cancelAnimationFrame(frame.current);
    frame.current = null;
    if (lastSent.current !== pending.current) {
      lastSent.current = pending.current;
      sentAt.current = performance.now();
      callback.current(pending.current);
    }
  };
  const update = (next: number, precision = step) => {
    if (!Number.isFinite(next)) return;
    pending.current = boundedValue(next, min, max, precision);
    setLocal(pending.current);
    setText(String(pending.current));
    // A drag's input goes to the engine as it arrives: waiting for the next
    // animation frame dropped one of two moves that fell into one (slow)
    // frame, ~1 in 6 at 60 Hz (notes: reference/slider-drag-nr.md). Faster
    // input (high-rate mice) still waits for the frame.
    if (performance.now() - sentAt.current >= MIN_SEND_INTERVAL_MS) flush();
    else if (frame.current === null) frame.current = requestAnimationFrame(flush);
  };
  const begin = () => {
    active.current = true;
    setDragging(true);
    onInteraction?.(true);
  };
  const end = () => {
    flush();
    active.current = false;
    scrub.current = null;
    fine.current = null;
    setDragging(false);
    onInteraction?.(false);
  };
  const reset = () => {
    if (disabled) return;
    update(defaultValue);
    end();
  };
  const keys = (event: KeyboardEvent<HTMLInputElement>) => {
    if (
      ![
        "ArrowLeft",
        "ArrowRight",
        "ArrowUp",
        "ArrowDown",
        "Home",
        "End",
      ].includes(event.key)
    )
      return;
    event.preventDefault();
    event.stopPropagation();
    const delta = step * (event.shiftKey ? 10 : event.altKey ? 0.1 : 1);
    update(
      event.key === "Home"
        ? min
        : event.key === "End"
          ? max
          : local +
            (["ArrowRight", "ArrowUp"].includes(event.key) ? delta : -delta),
      event.altKey ? step / 10 : step,
    );
    flush();
  };
  const scrubStart = (e: PointerEvent<HTMLLabelElement>) => {
    if (disabled || e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    begin();
    scrub.current = { x: e.clientX, value: local };
  };
  const commitText = () => {
    if (canceled.current) {
      canceled.current = false;
      pending.current = value;
      setLocal(value);
      setText(String(value));
      active.current = false;
      return;
    }
    // A field left as it was sends nothing: a stored value outside the
    // slider's range (an older, wider range) must not be clamped by a focus.
    if (text === shownValue(pending.current, step)) {
      // untouched
    } else if (text.trim() && Number.isFinite(Number(text))) update(Number(text));
    else {
      pending.current = value;
      setLocal(value);
      setText(String(value));
    }
    end();
  };
  return (
    <div
      className={`adjustment-control ${disabled ? "is-disabled" : ""} ${dragging ? "is-active" : ""}`}
    >
      <label
        htmlFor={id}
        title={t("studio.adjustment.scrubHint")}
        onPointerDown={scrubStart}
        onPointerMove={(e) => {
          if (scrub.current)
            update(
              fromTrack(
                toTrack(scrub.current.value) +
                  (e.clientX - scrub.current.x) *
                    trackStep *
                    (e.shiftKey ? 10 : e.altKey ? 0.1 : 1),
              ),
              e.altKey ? step / 10 : step,
            );
        }}
        onPointerUp={end}
        onPointerCancel={end}
        onDoubleClick={reset}
      >
        {label}
      </label>
      <input
        id={id}
        className={`adjustment-track ${track || ""}`}
        type="range"
        min={trackMin}
        max={trackMax}
        step={scale ? "any" : step}
        value={toTrack(local)}
        disabled={disabled}
        onChange={(e) => update(fromTrack(Number(e.target.value)))}
        onPointerDown={(e) => {
          const box = e.currentTarget.getBoundingClientRect();
          const travel = box.width - THUMB;
          if (!disabled && e.button === 0 && travel > 0) {
            // The drag is ours, not the native slider's: pressing the thumb
            // no longer moves it to the pointer (a click changed the value by
            // up to half a thumb), and the drag can slow down.
            e.preventDefault();
            // preventDefault also stops the native focus move: keys go here next.
            e.currentTarget.focus();
            e.currentTarget.setPointerCapture(e.pointerId);
            const left = (box.left || 0) + THUMB / 2;
            // The interaction starts before any value is sent: a jump to the
            // pointer is part of the drag (one history step), not a step of
            // its own before it.
            begin();
            // pending, not local: focusing the track just committed a typed value
            // whose state has not rendered yet.
            let start = toTrack(pending.current);
            const thumb = left + ((start - trackMin) / (trackMax - trackMin)) * travel;
            // Option grabs the thumb wherever the press is, as on the label.
            if (!e.altKey && Math.abs(e.clientX - thumb) > GRAB) {
              // A press beside the thumb moves it there, as a slider does.
              const at = Math.max(0, Math.min(1, (e.clientX - left) / travel));
              start = trackMin + at * (trackMax - trackMin);
              update(fromTrack(start));
            }
            fine.current = {
              x: e.clientX,
              centreY: (box.top || 0) + box.height / 2,
              value: start,
              perPixel: (trackMax - trackMin) / travel,
            };
          } else begin();
        }}
        onPointerMove={(e) => {
          if (!fine.current) return;
          const drag = fine.current;
          const factor = e.altKey ? 0.1 : dragPrecision(Math.abs(e.clientY - drag.centreY));
          // Each move adds its own distance at the precision of the moment:
          // moving away from the track never makes the value jump.
          drag.value = Math.max(
            trackMin,
            Math.min(trackMax, drag.value + (e.clientX - drag.x) * drag.perPixel * factor),
          );
          drag.x = e.clientX;
          update(fromTrack(drag.value), factor < 1 ? step / 10 : step);
        }}
        onPointerUp={end}
        onPointerCancel={end}
        onBlur={() => {
          if (active.current) end();
        }}
        onKeyDown={keys}
        onDoubleClick={reset}
        aria-label={ariaLabel ?? label}
        title={t("studio.adjustment.trackHint")}
      />
      <input
        className="adjustment-value"
        aria-label={
          unit
            ? t("studio.adjustment.valueWithUnit", { name: ariaLabel ?? label, unit })
            : t("studio.adjustment.value", { name: ariaLabel ?? label })
        }
        inputMode="decimal"
        type="text"
        value={text}
        disabled={disabled}
        onMouseDown={selectOnFocus.onMouseDown}
        onMouseUp={selectOnFocus.onMouseUp}
        onFocus={(e) => {
          active.current = true;
          selectOnFocus.onFocus(e);
        }}
        onChange={(e) => setText(e.target.value)}
        onBlur={commitText}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") {
            canceled.current = true;
            e.currentTarget.blur();
          }
        }}
      />
    </div>
  );
});
