import {
  memo,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import type { HueSatLum } from "../../utils/adjustments";

/** Hue 0 (red) at the right, increasing counter-clockwise, as in Lightroom's
 * colour grading wheels; saturation 0–100 from the centre to the rim. */
export function wheelPosition(hue: number, saturation: number) {
  const angle = (hue * Math.PI) / 180;
  const radius = Math.max(0, Math.min(100, saturation)) / 100;
  return { x: Math.cos(angle) * radius, y: -Math.sin(angle) * radius };
}

export function wheelValue(x: number, y: number) {
  const radius = Math.min(1, Math.hypot(x, y));
  const hue = ((Math.atan2(-y, x) * 180) / Math.PI + 360) % 360;
  return { hue: Math.round(hue), saturation: Math.round(radius * 100) };
}

interface Props {
  label: string;
  value: HueSatLum;
  disabled?: boolean;
  /** Diameter in pixels; "fill" sizes the wheel by its stylesheet (the
   * three-way view scales the wheels with the panel width). */
  size?: number | "fill";
  className?: string;
  style?: CSSProperties;
  onChange(value: HueSatLum): void;
  onInteraction?(active: boolean): void;
}

/**
 * A colour-grading wheel: drag the point for hue and saturation (Shift moves
 * at a quarter of the speed, Alt keeps the hue), arrow keys step, double-click
 * resets. At most one engine update per frame while dragging; luminance is
 * passed through unchanged.
 */
export default memo(function StudioColorWheel({
  label,
  value,
  disabled = false,
  size = 132,
  className = "",
  style,
  onChange,
  onInteraction,
}: Props) {
  const { t } = useTranslation();
  const [local, setLocal] = useState(value);
  const drag = useRef<{
    x: number;
    y: number;
    start: { x: number; y: number };
  } | null>(null);
  const pending = useRef(value);
  const frame = useRef<number | null>(null);
  const callback = useRef(onChange);
  callback.current = onChange;
  useEffect(() => {
    if (!drag.current) {
      setLocal(value);
      pending.current = value;
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
  const update = (next: HueSatLum) => {
    pending.current = next;
    setLocal(next);
    if (frame.current === null) frame.current = requestAnimationFrame(flush);
  };
  const pointAt = (e: PointerEvent<HTMLDivElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    return {
      x: ((e.clientX - box.left) / box.width) * 2 - 1,
      y: ((e.clientY - box.top) / box.height) * 2 - 1,
    };
  };
  const move = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    const p = pointAt(e);
    const speed = e.shiftKey ? 0.25 : 1;
    const x = d.start.x + (p.x - d.x) * speed;
    const y = d.start.y + (p.y - d.y) * speed;
    const next = wheelValue(x, y);
    update({
      ...pending.current,
      hue: e.altKey ? pending.current.hue : next.hue,
      saturation: next.saturation,
    });
  };
  const start = (e: PointerEvent<HTMLDivElement>) => {
    if (disabled || e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    const p = pointAt(e);
    const onPoint =
      Math.hypot(
        p.x - wheelPosition(local.hue, local.saturation).x,
        p.y - wheelPosition(local.hue, local.saturation).y,
      ) < 0.18;
    // A press beside the point jumps there (Lightroom); on the point it
    // drags relative to it, so fine Shift moves do not jump.
    const startPoint = onPoint
      ? wheelPosition(local.hue, local.saturation)
      : { x: p.x, y: p.y };
    drag.current = { x: p.x, y: p.y, start: startPoint };
    onInteraction?.(true);
    if (!onPoint) {
      const next = wheelValue(p.x, p.y);
      update({ ...local, hue: next.hue, saturation: next.saturation });
    }
  };
  const end = () => {
    if (!drag.current) return;
    drag.current = null;
    if (frame.current !== null) {
      cancelAnimationFrame(frame.current);
      flush();
    }
    onInteraction?.(false);
  };
  const keys = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 10 : 1;
    let next: HueSatLum | null = null;
    if (e.key === "ArrowLeft")
      next = { ...local, hue: (local.hue + 360 - step) % 360 };
    if (e.key === "ArrowRight")
      next = { ...local, hue: (local.hue + step) % 360 };
    if (e.key === "ArrowUp")
      next = { ...local, saturation: Math.min(100, local.saturation + step) };
    if (e.key === "ArrowDown")
      next = { ...local, saturation: Math.max(0, local.saturation - step) };
    if (!next) return;
    e.preventDefault();
    e.stopPropagation();
    update(next);
  };
  const point = wheelPosition(local.hue, local.saturation);
  const tint = `hsl(${local.hue} ${Math.round(40 + local.saturation * 0.6)}% 55%)`;
  return (
    <div
      className={`studio-wheel-disc ${disabled ? "is-disabled" : ""} ${className}`}
      style={size === "fill" ? style : { width: size, height: size, ...style }}
      role="slider"
      tabIndex={disabled ? -1 : 0}
      aria-label={t("studio.color.wheel.aria", { range: label })}
      aria-valuetext={t("studio.color.wheel.valueText", {
        hue: local.hue,
        saturation: local.saturation,
      })}
      aria-valuenow={local.saturation}
      aria-valuemin={0}
      aria-valuemax={100}
      title={t("studio.color.wheel.title")}
      onPointerDown={start}
      onPointerMove={move}
      onPointerUp={end}
      onPointerCancel={end}
      onKeyDown={keys}
      onDoubleClick={() => {
        if (disabled) return;
        update({ ...local, hue: 0, saturation: 0 });
      }}
    >
      <span
        className="studio-wheel-point"
        style={{
          left: `${50 + point.x * 50}%`,
          top: `${50 + point.y * 50}%`,
          background: local.saturation > 0 ? tint : undefined,
        }}
      />
    </div>
  );
});
