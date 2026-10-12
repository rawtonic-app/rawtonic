import {
  memo,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { useTranslation } from "react-i18next";

/**
 * Geometry of a three-way colour-grading cell, in SVG user units: the wheel
 * (radius DISC_R) and, concentric on its right, the luminance arc (radius
 * ARC_R) spanning ±ARC_SPAN degrees. The view box crops the unused left
 * margin so the wheel gets as much of the cell as possible.
 */
const CX = 50;
const CY = 50;
const DISC_R = 40;
const ARC_R = 47;
const ARC_SPAN = 50;
const VIEW = { x: 8, y: 8, w: 96, h: 84 };
export const THREE_WAY_ASPECT = `${VIEW.w} / ${VIEW.h}`;
/** Where the wheel sits inside the cell (percentages of the cell box). */
export const THREE_WAY_DISC = {
  left: `${((CX - DISC_R - VIEW.x) / VIEW.w) * 100}%`,
  top: `${((CY - DISC_R - VIEW.y) / VIEW.h) * 100}%`,
  width: `${((2 * DISC_R) / VIEW.w) * 100}%`,
  height: `${((2 * DISC_R) / VIEW.h) * 100}%`,
} as const;

const clamp = (v: number) => Math.max(-100, Math.min(100, Math.round(v)));
const angleOf = (value: number) => (clamp(value) / 100) * ARC_SPAN;
const pointOf = (degrees: number) => {
  const a = (degrees * Math.PI) / 180;
  return { x: CX + Math.cos(a) * ARC_R, y: CY - Math.sin(a) * ARC_R };
};
/** Luminance for an angle in degrees (0 at the right, up positive). */
export const arcValue = (degrees: number) => clamp((degrees / ARC_SPAN) * 100);
export const formatLuminance = (v: number) =>
  v > 0 ? `+${v}` : v < 0 ? `−${-v}` : "0";

const arcPath = (from: number, to: number) => {
  const a = pointOf(from);
  const b = pointOf(to);
  // Up the right side is counter-clockwise on screen (sweep 0).
  const sweep = to > from ? 0 : 1;
  return `M ${a.x} ${a.y} A ${ARC_R} ${ARC_R} 0 0 ${sweep} ${b.x} ${b.y}`;
};
const TRACK = arcPath(-ARC_SPAN, ARC_SPAN);

interface Props {
  /** The tonal range's name, for the accessible label. */
  label: string;
  value: number;
  tint?: string;
  disabled?: boolean;
  onChange(value: number): void;
  onInteraction?(active: boolean): void;
}

/**
 * Capture One's three-way luminance control: an arc beside the wheel, up
 * brightens the range, down darkens it. Drag (Shift at a quarter of the
 * speed; a press away from the knob jumps there), arrow keys step (Shift ×10),
 * Home/End go to the ends, double-click resets. At most one engine update
 * per frame while dragging; the value shows beside the knob meanwhile.
 */
export default memo(function StudioLuminanceArc({
  label,
  value,
  tint,
  disabled = false,
  onChange,
  onInteraction,
}: Props) {
  const { t } = useTranslation();
  const [local, setLocal] = useState(clamp(value));
  const [dragging, setDragging] = useState(false);
  const drag = useRef<{ angle: number; start: number } | null>(null);
  const pending = useRef(local);
  const frame = useRef<number | null>(null);
  const callback = useRef(onChange);
  callback.current = onChange;
  useEffect(() => {
    if (!drag.current) {
      setLocal(clamp(value));
      pending.current = clamp(value);
    }
  }, [value]);
  useEffect(
    () => () => {
      if (frame.current !== null) cancelAnimationFrame(frame.current);
    },
    [],
  );
  const flush = () => {
    frame.current = null;
    callback.current(pending.current);
  };
  const update = (next: number) => {
    pending.current = next;
    setLocal(next);
    if (frame.current === null) frame.current = requestAnimationFrame(flush);
  };
  const commit = (next: number) => {
    if (frame.current !== null) {
      cancelAnimationFrame(frame.current);
      frame.current = null;
    }
    pending.current = next;
    setLocal(next);
    callback.current(next);
  };
  const angleAt = (e: PointerEvent<SVGElement>) => {
    const svg = (e.currentTarget as SVGElement).ownerSVGElement;
    const box = (svg ?? e.currentTarget).getBoundingClientRect();
    if (!box.width || !box.height) return 0;
    const x = VIEW.x + ((e.clientX - box.left) / box.width) * VIEW.w;
    const y = VIEW.y + ((e.clientY - box.top) / box.height) * VIEW.h;
    return (Math.atan2(-(y - CY), x - CX) * 180) / Math.PI;
  };
  const start = (e: PointerEvent<SVGElement>) => {
    if (disabled || e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.setPointerCapture?.(e.pointerId);
    (e.currentTarget as SVGElement).focus?.({ preventScroll: true });
    const angle = angleAt(e);
    // On the knob the drag is relative to it, so Shift moves do not jump;
    // elsewhere on the arc the knob jumps to the pointer.
    const onKnob = Math.abs(angle - angleOf(local)) < 7;
    const startValue = onKnob ? local : arcValue(angle);
    drag.current = { angle, start: startValue };
    setDragging(true);
    onInteraction?.(true);
    if (!onKnob) update(startValue);
  };
  const move = (e: PointerEvent<SVGElement>) => {
    const d = drag.current;
    if (!d) return;
    const speed = e.shiftKey ? 0.25 : 1;
    const delta = ((angleAt(e) - d.angle) / ARC_SPAN) * 100 * speed;
    const next = clamp(d.start + delta);
    if (next !== pending.current) update(next);
  };
  const end = () => {
    if (!drag.current) return;
    drag.current = null;
    setDragging(false);
    if (frame.current !== null) {
      cancelAnimationFrame(frame.current);
      flush();
    }
    onInteraction?.(false);
  };
  const interaction = useRef(onInteraction);
  interaction.current = onInteraction;
  // An arc that vanishes mid-drag (view switched) ends its interaction.
  useEffect(
    () => () => {
      if (drag.current) interaction.current?.(false);
    },
    [],
  );
  const keys = (e: KeyboardEvent<SVGElement>) => {
    if (disabled) return;
    const step = e.shiftKey ? 10 : 1;
    let next: number | null = null;
    if (e.key === "ArrowUp" || e.key === "ArrowRight") next = local + step;
    if (e.key === "ArrowDown" || e.key === "ArrowLeft") next = local - step;
    if (e.key === "PageUp") next = local + 10;
    if (e.key === "PageDown") next = local - 10;
    if (e.key === "Home") next = -100;
    if (e.key === "End") next = 100;
    if (next === null) return;
    e.preventDefault();
    e.stopPropagation();
    if (clamp(next) !== local) commit(clamp(next));
  };
  const angle = angleOf(local);
  const knob = pointOf(angle);
  const text = formatLuminance(local);
  return (
    <>
      <svg
        className={`studio-lum-arc ${disabled ? "is-disabled" : ""}`}
        viewBox={`${VIEW.x} ${VIEW.y} ${VIEW.w} ${VIEW.h}`}
      >
        <path className="studio-lum-arc-track" d={TRACK} />
        <circle
          className="studio-lum-arc-zero"
          cx={pointOf(0).x}
          cy={pointOf(0).y}
          r={1.1}
        />
        {local !== 0 && (
          <path className="studio-lum-arc-fill" d={arcPath(0, angle)} />
        )}
        <circle
          className="studio-lum-arc-knob"
          cx={knob.x}
          cy={knob.y}
          r={3.6}
          style={tint ? { fill: tint } : undefined}
        />
        <path
          className="studio-lum-arc-hit"
          d={TRACK}
          role="slider"
          tabIndex={disabled ? -1 : 0}
          aria-label={t("studio.color.grading.luminanceAria", { range: label })}
          aria-orientation="vertical"
          aria-valuemin={-100}
          aria-valuemax={100}
          aria-valuenow={local}
          aria-valuetext={text}
          aria-disabled={disabled || undefined}
          onPointerDown={start}
          onPointerMove={move}
          onPointerUp={end}
          onPointerCancel={end}
          onKeyDown={keys}
          onDoubleClick={() => {
            if (!disabled) commit(0);
          }}
        >
          <title>
            {t("studio.color.grading.luminanceArcTitle", {
              range: label,
              value: text,
            })}
          </title>
        </path>
      </svg>
      {dragging && (
        <span
          className="studio-lum-arc-value"
          style={{
            left: `${((knob.x - VIEW.x) / VIEW.w) * 100}%`,
            top: `${((knob.y - VIEW.y) / VIEW.h) * 100}%`,
          }}
          role="status"
        >
          {text}
        </span>
      )}
    </>
  );
});
