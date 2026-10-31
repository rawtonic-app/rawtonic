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
  // An Option-drag on the track: a tenth of the track's own movement per pixel.
  const fine = useRef<{ x: number; value: number; perPixel: number } | null>(null);
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
    if (text.trim() && Number.isFinite(Number(text))) update(Number(text));
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
          if (e.altKey && !disabled && e.button === 0) {
            // Fine control (Option, as on the label): the thumb does not jump
            // to the pointer and moves a tenth of a plain drag per pixel.
            e.preventDefault();
            // preventDefault also stops the native focus move: keys go here next.
            e.currentTarget.focus();
            e.currentTarget.setPointerCapture(e.pointerId);
            const width = e.currentTarget.getBoundingClientRect().width;
            fine.current = {
              x: e.clientX,
              value: toTrack(local),
              perPixel: width > 0 ? (trackMax - trackMin) / width / 10 : 0,
            };
          }
          begin();
        }}
        onPointerMove={(e) => {
          if (!fine.current) return;
          const { x, value, perPixel } = fine.current;
          update(
            fromTrack(Math.max(trackMin, Math.min(trackMax, value + (e.clientX - x) * perPixel))),
            step / 10,
          );
        }}
        onPointerUp={end}
        onPointerCancel={end}
        onBlur={() => {
          if (active.current) end();
        }}
        onKeyDown={keys}
        onDoubleClick={reset}
        aria-label={ariaLabel ?? label}
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
