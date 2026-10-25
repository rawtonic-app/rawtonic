import {
  memo,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import type { ColorRange } from "../../utils/adjustments";
import { oklchCss } from "../../utils/oklab";

/**
 * Geometry of the range wheel: Oklab hue around (0 at the right, increasing
 * counter-clockwise like the colour-grading wheels), chroma outwards. The
 * radius grows with the square root of chroma so the low chroma of skin and
 * foliage gets room; the rim is RIM_CHROMA.
 */
export const RIM_CHROMA = 0.32;
const VIEW = 1.12;

const clampTo = (v: number, lo: number, hi: number) =>
  Math.max(lo, Math.min(hi, v));
/** Signed hue difference a − b in degrees, −180…180. */
const hueDelta = (a: number, b: number) => ((a - b + 540) % 360) - 180;

export const radiusOf = (chroma: number) =>
  Math.sqrt(clampTo(chroma, 0, RIM_CHROMA) / RIM_CHROMA);
export const chromaOf = (radius: number) =>
  clampTo(radius, 0, 1) ** 2 * RIM_CHROMA;

export function wheelPoint(hue: number, chroma: number) {
  const a = (hue * Math.PI) / 180;
  const r = radiusOf(chroma);
  return { x: Math.cos(a) * r, y: -Math.sin(a) * r };
}

export function wheelColor(x: number, y: number) {
  return {
    hue: ((Math.atan2(-y, x) * 180) / Math.PI + 360) % 360,
    chroma: chromaOf(Math.hypot(x, y)),
  };
}

/** The chroma the range's band is built around (greys still get a band). */
const centreChroma = (range: ColorRange) => Math.max(range.chroma, 0.005);

/**
 * The range's selection as the renderer builds it (image_processing.rs
 * `pack_color_ranges`, shader `color_range_weight`): full weight within
 * ±hueRange and the chroma band, falling off over the feather.
 */
export function rangeBand(range: ColorRange) {
  const c = centreChroma(range);
  const cr = clampTo(range.chromaRange, 0, 100) / 100;
  const soft = 0.1 + (0.9 * clampTo(range.smoothness, 0, 100)) / 100;
  const lo = c * (1 - cr);
  const hi = c * (1 + 2 * cr) + 0.01;
  const chromaFeather = 0.5 * soft * (hi - lo) + 0.004;
  const half = clampTo(range.hueRange, 5, 90);
  return {
    hueLo: range.hue - half,
    hueHi: range.hue + half,
    hueFeather: half * soft,
    chromaLo: lo,
    chromaHi: hi,
    chromaFeather,
  };
}

/** Hue range (half-width, 5–90°) that puts a hue edge at `edgeHue`. */
export const hueRangeForEdge = (range: ColorRange, edgeHue: number) =>
  Math.round(clampTo(Math.abs(hueDelta(edgeHue, range.hue)), 5, 90));

/** Saturation range (0–100) that puts the band's inner edge at `chroma`. */
export const chromaRangeForInner = (range: ColorRange, chroma: number) =>
  Math.round(clampTo((1 - chroma / centreChroma(range)) * 100, 0, 100));

/** Saturation range (0–100) that puts the band's outer edge at `chroma`. */
export const chromaRangeForOuter = (range: ColorRange, chroma: number) => {
  const c = centreChroma(range);
  return Math.round(clampTo(((chroma - 0.01 - c) / (2 * c)) * 100, 0, 100));
};

/** Annular sector from hue a to b (a < b) between two chroma radii. */
function sectorPath(a: number, b: number, chromaLo: number, chromaHi: number) {
  const span = Math.min(b - a, 359.9);
  const r0 = radiusOf(Math.max(chromaLo, 0));
  const r1 = radiusOf(chromaHi);
  const at = (hue: number, r: number) => {
    const t = (hue * Math.PI) / 180;
    return `${(Math.cos(t) * r).toFixed(4)} ${(-Math.sin(t) * r).toFixed(4)}`;
  };
  const large = span > 180 ? 1 : 0;
  // Counter-clockwise on screen is sweep 0 (y points down).
  return [
    `M ${at(a, r1)}`,
    `A ${r1} ${r1} 0 ${large} 0 ${at(a + span, r1)}`,
    `L ${at(a + span, r0)}`,
    r0 > 0 ? `A ${r0} ${r0} 0 ${large} 1 ${at(a, r0)}` : "",
    "Z",
  ].join(" ");
}

type Handle = "colour" | "hueLo" | "hueHi" | "chromaLo" | "chromaHi";

interface Props {
  /** All ranges shown (the active one editable, the others faint). */
  ranges: ColorRange[];
  active?: ColorRange;
  disabled?: boolean;
  /** A patch for the active range. */
  onChange(patch: Partial<ColorRange>): void;
  /** A press on the wheel with no range yet: pick that colour. */
  onPick?(hue: number, chroma: number): void;
  onInteraction?(active: boolean): void;
}

/**
 * The colour editor's range view: every range's slice on a hue/chroma
 * wheel. Drag a hue edge or a chroma edge of the active slice to reshape
 * it, drag its centre (or press anywhere on the wheel) to move it to
 * another colour; arrow keys step each handle. Synced with the range
 * sliders through the range itself; at most one engine update per frame.
 */
export default memo(function StudioColorRangeWheel({
  ranges,
  active,
  disabled = false,
  onChange,
  onPick,
  onInteraction,
}: Props) {
  const { t } = useTranslation();
  const [local, setLocal] = useState(active);
  const drag = useRef<Handle | null>(null);
  const pending = useRef<Partial<ColorRange>>({});
  const frame = useRef<number | null>(null);
  const callback = useRef(onChange);
  callback.current = onChange;
  useEffect(() => {
    if (!drag.current) setLocal(active);
  }, [active]);
  const interaction = useRef(onInteraction);
  interaction.current = onInteraction;
  useEffect(
    () => () => {
      if (frame.current !== null) cancelAnimationFrame(frame.current);
      if (drag.current) interaction.current?.(false);
    },
    [],
  );
  const flush = () => {
    frame.current = null;
    const patch = pending.current;
    pending.current = {};
    if (Object.keys(patch).length) callback.current(patch);
  };
  const update = (patch: Partial<ColorRange>) => {
    pending.current = { ...pending.current, ...patch };
    setLocal((r) => r && { ...r, ...patch });
    if (frame.current === null) frame.current = requestAnimationFrame(flush);
  };
  const commit = (patch: Partial<ColorRange>) => {
    if (frame.current !== null) {
      cancelAnimationFrame(frame.current);
      frame.current = null;
    }
    pending.current = { ...pending.current, ...patch };
    setLocal((r) => r && { ...r, ...patch });
    flush();
  };
  const pointAt = (e: PointerEvent<SVGElement>) => {
    const svg =
      e.currentTarget instanceof SVGSVGElement
        ? e.currentTarget
        : (e.currentTarget as SVGElement).ownerSVGElement;
    const box = (svg ?? e.currentTarget).getBoundingClientRect();
    if (!box.width || !box.height) return { x: 0, y: 0 };
    return {
      x: (((e.clientX - box.left) / box.width) * 2 - 1) * VIEW,
      y: (((e.clientY - box.top) / box.height) * 2 - 1) * VIEW,
    };
  };
  const patchFor = (
    handle: Handle,
    range: ColorRange,
    x: number,
    y: number,
  ) => {
    const { hue, chroma } = wheelColor(x, y);
    // A chroma edge follows the pointer's distance along the slice's centre
    // line; a hue edge its angle.
    const along = Math.max(
      0,
      x * Math.cos((range.hue * Math.PI) / 180) -
        y * Math.sin((range.hue * Math.PI) / 180),
    );
    switch (handle) {
      case "colour":
        return {
          hue: Math.round(hue * 10) / 10,
          chroma: Math.round(chroma * 1000) / 1000,
        };
      case "hueLo":
      case "hueHi":
        return { hueRange: hueRangeForEdge(range, hue) };
      case "chromaLo":
        return { chromaRange: chromaRangeForInner(range, chromaOf(along)) };
      case "chromaHi":
        return { chromaRange: chromaRangeForOuter(range, chromaOf(along)) };
    }
  };
  const start = (handle: Handle | null) => (e: PointerEvent<SVGElement>) => {
    if (disabled || e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.setPointerCapture?.(e.pointerId);
    const p = pointAt(e);
    if (!local) {
      const { hue, chroma } = wheelColor(p.x, p.y);
      onPick?.(Math.round(hue * 10) / 10, Math.round(chroma * 1000) / 1000);
      return;
    }
    // A press beside the handles moves the range there (and drags it on).
    drag.current = handle ?? "colour";
    onInteraction?.(true);
    if (!handle) update(patchFor("colour", local, p.x, p.y));
  };
  const move = (e: PointerEvent<SVGElement>) => {
    const handle = drag.current;
    if (!handle || !local) return;
    const p = pointAt(e);
    update(patchFor(handle, local, p.x, p.y));
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
  const keys = (handle: Handle) => (e: KeyboardEvent<SVGElement>) => {
    if (disabled || !local) return;
    const step = e.shiftKey ? 10 : 1;
    const up = e.key === "ArrowUp" || e.key === "ArrowRight";
    const down = e.key === "ArrowDown" || e.key === "ArrowLeft";
    if (!up && !down) return;
    e.preventDefault();
    e.stopPropagation();
    const sign = up ? 1 : -1;
    if (handle === "colour") {
      // Left/right turn the hue, up/down change the chroma.
      if (e.key === "ArrowLeft" || e.key === "ArrowRight")
        commit({ hue: (local.hue + sign * step + 360) % 360 });
      else
        commit({
          chroma:
            Math.round(
              clampTo(local.chroma + sign * step * 0.002, 0, RIM_CHROMA) * 1000,
            ) / 1000,
        });
    } else if (handle === "hueLo" || handle === "hueHi") {
      commit({ hueRange: clampTo(local.hueRange + sign * step, 5, 90) });
    } else {
      commit({ chromaRange: clampTo(local.chromaRange + sign * step, 0, 100) });
    }
  };
  const handleProps = (handle: Handle) => ({
    tabIndex: disabled ? -1 : 0,
    role: "slider",
    "aria-disabled": disabled || undefined,
    onPointerDown: start(handle),
    onPointerMove: move,
    onPointerUp: end,
    onPointerCancel: end,
    onKeyDown: keys(handle),
  });
  const band = local && rangeBand(local);
  const mid = band && (radiusOf(band.chromaLo) + radiusOf(band.chromaHi)) / 2;
  const polar = (hue: number, r: number) => {
    const a = (hue * Math.PI) / 180;
    return { cx: Math.cos(a) * r, cy: -Math.sin(a) * r };
  };
  return (
    <div
      className={`studio-range-wheel ${disabled ? "is-disabled" : ""}`}
      aria-label={t("studio.color.editor.wheel.aria")}
      role="group"
    >
      <svg
        viewBox={`${-VIEW} ${-VIEW} ${2 * VIEW} ${2 * VIEW}`}
        onPointerDown={start(null)}
        onPointerMove={move}
        onPointerUp={end}
        onPointerCancel={end}
      >
        <title>{t("studio.color.editor.wheel.title")}</title>
        <circle className="studio-range-wheel-rim" r={1} />
        {ranges
          .filter((r) => r.id !== local?.id)
          .map((r) => {
            const b = rangeBand(r);
            return (
              <path
                key={r.id}
                className="studio-range-wheel-other"
                d={sectorPath(b.hueLo, b.hueHi, b.chromaLo, b.chromaHi)}
              />
            );
          })}
        {local && band && mid !== undefined && (
          <>
            <path
              className="studio-range-wheel-feather"
              d={sectorPath(
                band.hueLo - band.hueFeather,
                band.hueHi + band.hueFeather,
                band.chromaLo - band.chromaFeather,
                band.chromaHi + band.chromaFeather,
              )}
            />
            <path
              className="studio-range-wheel-slice"
              d={sectorPath(
                band.hueLo,
                band.hueHi,
                band.chromaLo,
                band.chromaHi,
              )}
            />
            {(["hueLo", "hueHi"] as const).map((h) => (
              <circle
                key={h}
                className="studio-range-wheel-edge"
                r={0.055}
                {...polar(band[h], mid)}
                {...handleProps(h)}
                aria-label={t(`studio.color.editor.wheel.${h}`)}
                aria-valuemin={5}
                aria-valuemax={90}
                aria-valuenow={local.hueRange}
              />
            ))}
            {(["chromaLo", "chromaHi"] as const).map((h) => (
              <circle
                key={h}
                className="studio-range-wheel-edge is-chroma"
                r={0.05}
                {...polar(local.hue, radiusOf(Math.max(band[h], 0)))}
                {...handleProps(h)}
                aria-label={t(`studio.color.editor.wheel.${h}`)}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={local.chromaRange}
              />
            ))}
            <circle
              className="studio-range-wheel-colour"
              r={0.07}
              {...polar(local.hue, radiusOf(local.chroma))}
              style={{
                fill: oklchCss(local.lightness, local.chroma, local.hue),
              }}
              {...handleProps("colour")}
              aria-label={t("studio.color.editor.wheel.colour")}
              aria-valuemin={0}
              aria-valuemax={360}
              aria-valuenow={Math.round(local.hue)}
              aria-valuetext={t("studio.color.editor.wheel.colourText", {
                hue: Math.round(local.hue),
                chroma: Math.round(local.chroma * 1000) / 10,
              })}
            />
          </>
        )}
      </svg>
    </div>
  );
});
