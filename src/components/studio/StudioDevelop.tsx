import { cropResetValues } from "../../utils/cameraCrop";
import { memo, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useShallow } from "zustand/react/shallow";
import { useTranslation } from "react-i18next";
import {
  ChevronDown,
  RotateCcw,
  Pipette,
  SlidersHorizontal,
  Palette,
  Focus,
  Crop,
  Layers,
  Eye,
  EyeOff,
  Sparkles,
  GripVertical,
  Crosshair,
  ScanFace,
} from "lucide-react";
import { debouncedSetHistory, panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { useDragSend } from "../../hooks/useDragSend";
import { useSliderDraft } from "../../utils/sliderDraft";
import {
  INITIAL_ADJUSTMENTS,
  initialAdjustmentsFor,
  BRIGHTNESS_CONTROL,
  MAX_CURVE_POINTS,
  INITIAL_REGION_CURVE,
  LONG_CURVE_VERSION,
  editedCurvesVersion,
  type Adjustments,
  type Coord,
  type Curves,
  type GrainStructure,
  effectiveGrainStructure,
  isFilmGrain,
  withNewGrainStructure,
  type RegionCurve,
} from "../../utils/adjustments";
import {
  Panel,
  type AppSettings,
  type MaskDisplay,
} from "../ui/AppProperties";
import {
  DEFAULT_MASK_OPACITY,
  MASK_COLOR_PRESETS,
  MASK_DISPLAY_OPTIONS,
  chooseMaskDisplay,
  maskColorOf,
  maskDisplayOf,
  maskOpacityOf,
} from "../../utils/maskDisplay";
import AdjustmentControl from "./AdjustmentControl";
import StudioCameraProfile from "./StudioCameraProfile";
import StudioWhiteBalance from "./StudioWhiteBalance";
import type {
  CurveChannel,
  TatTarget,
} from "../../utils/targetedAdjustment";
import {
  pointCurveValue,
  renderedLuminancePoints,
  renderedPoints,
} from "../../utils/pointCurve";
import StudioHistogram from "./StudioHistogram";
import { PerfProfiler } from "../../hooks/usePerformanceDiagnostics";
import StudioLayers, { NewLayerButton } from "./StudioLayers";
import StudioLut from "./StudioLut";
import StudioRelight from "./StudioRelight";
import { setRelightPanelOpen } from "../../store/useRelightStore";
import StudioCropControls from "./StudioCropControls";
import StudioPresets from "./StudioPresets";
import StudioLensCorrection from "./StudioLensCorrection";
import StudioUpright from "./StudioUpright";
import StudioFaceRetouch from "./StudioFaceRetouch";
import { StudioSpotPanel, resetSpotSettings } from "./StudioSpotControls";
import StudioToolCopy, { type ToolParams } from "./StudioToolCopy";
import StudioToolPresets from "./StudioToolPresets";
import { PRESET_TOOLS } from "./toolPresets";
import { ToolColumn, useToolMenu, useToolOpen, useToolOpenOnly } from "./toolLayout";
import StudioHistory from "./StudioHistory";
import StudioSnapshots from "./StudioSnapshots";
import {
  StudioBlackAndWhite,
  StudioCalibration,
  StudioColorGrading,
} from "./StudioColorTools";
import { RegionCurveControls, StudioDefringe } from "./StudioToneTools";
import { StudioLevels } from "./StudioLevels";
import StudioColorEditor from "./StudioColorEditor";
import {
  isNeutralRegionCurve,
  regionCurveValue,
} from "../../utils/regionCurve";
import { curvePointerPosition } from "../../utils/curvePointer";
import { selectOnFocus } from "../../utils/selectOnFocus";

export function ToolGroup({
  title,
  label,
  children,
  open = true,
  onReset,
  copy,
  toggle,
  actions,
  onOpenChange,
  expand,
}: {
  /** Stable id of the tool (remembered open state and order, copy/apply,
   * `data-tool-title`); never translated. */
  title: string;
  onOpenChange?: (open: boolean) => void;
  /** The shown, translated name; `title` when absent. */
  label?: string;
  children: ReactNode;
  /** Extra header buttons (before reset). */
  actions?: ReactNode;
  open?: boolean;
  onReset?: () => void;
  /** Parameters for Capture One's per-tool copy/apply (photo only); tools
   * in `PRESET_TOOLS` also get a preset menu for them. */
  copy?: ToolParams;
  /** Lightroom's eye: the tool's effect on or off without losing values. */
  toggle?: { enabled: boolean; onToggle(): void };
  /** Opens the group (remembered) when it turns true: its cursor tool was
   * chosen from the keyboard or the tool bar. */
  expand?: boolean;
}) {
  const { t } = useTranslation();
  const name = label ?? title;
  const [isOpen, setOpen] = useToolOpen(title, open);
  useEffect(() => {
    if (expand && !isOpen) setOpen(true);
    // Only the change of `expand` opens it: the user may close it again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expand]);
  useEffect(() => {
    onOpenChange?.(isOpen);
    return () => onOpenChange?.(false);
  }, [isOpen, onOpenChange]);
  const menu = useToolMenu();
  const openOnly = useToolOpenOnly();
  return (
    <details
      className={`studio-tool-group ${toggle && !toggle.enabled ? "is-off" : ""}`}
      open={isOpen}
      data-tool-title={title}
      onToggle={(e) => {
        // Native toggles (header clicks) are remembered for this column.
        if (e.currentTarget.open !== isOpen) setOpen(e.currentTarget.open);
      }}
    >
      <summary
        onContextMenu={menu}
        onClick={(e) => {
          // Option-click opens this tool alone (Lightroom's panel headers).
          if (!e.altKey || !openOnly) return;
          e.preventDefault();
          openOnly(title);
        }}
      >
        {toggle && (
          <button
            type="button"
            className="studio-tool-eye"
            title={
              toggle.enabled
                ? t("studio.develop.toolGroup.turnOff", { tool: name })
                : t("studio.develop.toolGroup.turnOn", { tool: name })
            }
            aria-label={t("studio.develop.toolGroup.isOn", { tool: name })}
            aria-pressed={toggle.enabled}
            onClick={(e) => {
              e.preventDefault();
              toggle.onToggle();
            }}
          >
            {toggle.enabled ? <Eye size={12} /> : <EyeOff size={12} />}
          </button>
        )}
        {menu && (
          // Tools in a column move by dragging their header.
          <GripVertical className="studio-tool-grip" size={11} aria-hidden />
        )}
        <ChevronDown size={12} />
        <span>{name}</span>
        {copy && PRESET_TOOLS.has(title) && <StudioToolPresets tool={title} label={name} params={copy} />}
        {copy && <StudioToolCopy tool={title} label={name} params={copy} />}
        {actions}
        {onReset && (
          <button
            type="button"
            title={t("studio.develop.toolGroup.reset", { tool: name })}
            aria-label={t("studio.develop.toolGroup.reset", { tool: name })}
            onClick={(e) => {
              e.preventDefault();
              onReset();
            }}
          >
            <RotateCcw size={12} />
          </button>
        )}
      </summary>
      <div className="studio-tool-body">
        <PerfProfiler id={`tool:${title}`}>{children}</PerfProfiler>
      </div>
    </details>
  );
}

/** The region curve under the luma curve; a region slider's drag draws it
 * at every move (from the drag's draft), re-rendering only this line; a
 * point drag does not re-render it. */
const RegionCurveLine = memo(function RegionCurveLine({ region }: { region: RegionCurve }) {
  const path = useEditorStore((s) => s.selectedImage?.path);
  const dragged = useSliderDraft(
    path,
    (adjustments) => adjustments.regionCurve as RegionCurve | undefined,
  );
  const shown = dragged ? { ...INITIAL_REGION_CURVE, ...dragged } : region;
  if (isNeutralRegionCurve(shown)) return null;
  return (
    <polyline
      points={Array.from({ length: 65 }, (_, i) => {
        const x = i / 64;
        return `${x * 255},${255 - regionCurveValue(shown, x) * 255}`;
      }).join(" ")}
      fill="none"
      className="curve-region"
    />
  );
});

/** The region curve's split points (percent of the input) and their ranges,
 * as the sliders they replace had them. */
type SplitKey = "shadowSplit" | "midtoneSplit" | "highlightSplit";
const SPLIT_KEYS: SplitKey[] = ["shadowSplit", "midtoneSplit", "highlightSplit"];
const SPLIT_RANGES: Record<SplitKey, [number, number]> = {
  shadowSplit: [5, 45],
  midtoneSplit: [25, 75],
  highlightSplit: [55, 95],
};
const SPLIT_LABELS = {
  shadowSplit: "studio.tone.regionCurve.aria.shadowSplit",
  midtoneSplit: "studio.tone.regionCurve.aria.midtoneSplit",
  highlightSplit: "studio.tone.regionCurve.aria.highlightSplit",
} as const;
/** Height of the split handles' strip under the plot, in curve units. */
const SPLIT_STRIP = 16;

export function PointCurve({
  curves,
  region,
  onChange,
  disabled,
  onInteraction,
  wholeCurves = false,
  targeted = false,
  onRegionChange,
}: {
  curves: Curves;
  /** Lightroom's region curve, drawn under the luma curve when set. */
  region?: RegionCurve;
  /** With `region`: its three split points as handles under the RGB curve. */
  onRegionChange?(region: RegionCurve): void;
  onChange(curves: Curves): void;
  disabled: boolean;
  onInteraction(active: boolean): void;
  /** Curves beyond 16 points render whole (the photo's curves at version 2). */
  wholeCurves?: boolean;
  /** The targeted curve acts on these curves: its input level under the
   * pointer is marked. */
  targeted?: boolean;
}) {
  const { t } = useTranslation();
  // The channel is the store's: the targeted curve moves the one shown.
  const channel = useEditorStore((s) => s.curveChannel);
  // The marked input was the previous channel's: the next hover marks anew.
  const setChannel = (curveChannel: CurveChannel) =>
    useEditorStore
      .getState()
      .setEditor({ curveChannel, tatCurveMarker: null });
  const marker = useEditorStore((s) =>
    targeted && s.tatTarget === "curve" ? s.tatCurveMarker : null,
  );
  const [selected, setSelected] = useState<number | null>(null);
  // A drag draws its points as they move, like a slider its knob; the
  // store gets them at the slider cadence and commits every
  // COMMIT_INTERVAL_MS (sliderDraft.ts), too seldom to draw from.
  const [dragged, setDragged] = useState<Coord[] | null>(null);
  // The split point being dragged, and the one whose exact value is typed.
  const [splitDrag, setSplitDrag] = useState<SplitKey | null>(null);
  // Its value while it moves: the handle draws from this, not from the
  // committed draft that lags up to COMMIT_INTERVAL_MS behind.
  const [splitLive, setSplitLive] = useState<number | null>(null);
  // A split drag still running when the curve unmounts (another photo or tool)
  // must give the interaction back.
  const splitActive = useRef(false);
  const interactionRef = useRef(onInteraction);
  interactionRef.current = onInteraction;
  useEffect(
    () => () => {
      if (splitActive.current) {
        splitActive.current = false;
        interactionRef.current(false);
      }
    },
    [],
  );
  const endSplit = () => {
    if (!splitActive.current) return;
    splitActive.current = false;
    setSplitDrag(null);
    setSplitLive(null);
    onInteraction(false);
  };
  const [splitEdit, setSplitEdit] = useState<SplitKey | null>(null);
  const histogram = useEditorStore((s) => s.histogram);
  // Curves without a Luminance curve start it neutral.
  const points = dragged ??
    curves[channel] ?? [
      { x: 0, y: 0 },
      { x: 255, y: 255 },
    ];
  const latest = useRef(points);
  latest.current = points;
  const send = useDragSend((next: Coord[]) =>
    onChange({ ...curves, [channel]: next }),
  );
  const svgRef = useRef<SVGSVGElement>(null);
  const dragging = useRef(false);
  const interaction = useRef(onInteraction);
  interaction.current = onInteraction;
  useEffect(
    () => () => {
      send.flush();
      if (dragging.current) interaction.current(false);
    },
    [send],
  );
  const end = () => {
    send.flush();
    if (dragging.current) interaction.current(false);
    dragging.current = false;
    setDragged(null);
  };
  const rendered = useMemo(
    () =>
      channel === "luminance"
        ? renderedLuminancePoints(points)
        : renderedPoints(points, wholeCurves),
    [channel, points, wholeCurves],
  );
  const drawn = useMemo(
    () =>
      Array.from({ length: 129 }, (_, i) => {
        const x = i * (255 / 128);
        return `${x},${255 - pointCurveValue(rendered, x)}`;
      }).join(" "),
    [rendered],
  );
  // The photo's histogram behind the curve (both reference editors), the
  // shown channel's; the RGB and Luminance curves show the luminance one.
  const histogramValues =
    histogram &&
    (channel === "red" || channel === "green" || channel === "blue"
      ? histogram[channel]
      : histogram.luma);
  const histogramPath = useMemo(() => {
    if (!histogramValues?.length) return null;
    const max = Math.max(1, ...histogramValues);
    const last = Math.max(1, histogramValues.length - 1);
    return `M0 255 ${histogramValues
      .map((v, i) => `L${((i * 255) / last).toFixed(2)} ${(255 - Math.sqrt(Math.max(0, v) / max) * 240).toFixed(2)}`)
      .join(" ")} L255 255 Z`;
  }, [histogramValues]);
  const splits = channel === "luma" && region && onRegionChange ? region : null;
  const setSplit = (key: SplitKey, value: number) => {
    if (!region || !onRegionChange) return;
    const [min, max] = SPLIT_RANGES[key];
    const next = Math.max(min, Math.min(max, Math.round(value)));
    if (splitActive.current) setSplitLive(next);
    if (next !== region[key]) onRegionChange({ ...region, [key]: next });
  };
  const position = (event: React.PointerEvent<SVGSVGElement>) => {
    const matrix = event.currentTarget.getScreenCTM();
    return matrix
      ? curvePointerPosition(event.clientX, event.clientY, matrix.inverse())
      : null;
  };
  const keyboardChange = (next: Coord[]) => {
    debouncedSetHistory.flush();
    onChange({ ...curves, [channel]: next });
    debouncedSetHistory.flush();
  };
  const focusPoint = (index: number) => {
    requestAnimationFrame(() =>
      svgRef.current
        ?.querySelector<SVGElement>(`[data-curve-point="${index}"]`)
        ?.focus(),
    );
  };
  return (
    <div className="studio-curve">
      <div
        className="studio-segments"
        aria-label={t("studio.develop.curve.channel")}
      >
        {(
          [
            ["luma", t("adjustments.curves.channels.luma")],
            ["luminance", t("adjustments.curves.channels.luminance")],
            ["red", "R"],
            ["green", "G"],
            ["blue", "B"],
          ] as const
        ).map(([key, label]) => (
          <button
            key={key}
            aria-pressed={channel === key}
            onClick={() => {
              setChannel(key);
              setSelected(null);
            }}
          >
            {label}
          </button>
        ))}
      </div>
      <svg
        ref={svgRef}
        viewBox={splits ? `0 0 255 ${255 + SPLIT_STRIP}` : "0 0 255 255"}
        role="group"
        aria-label={t("studio.develop.curve.aria", {
          channel: t(`studio.develop.curve.ariaChannels.${channel}`),
        })}
        onPointerDown={(e) => {
          if (disabled || e.button !== 0) return;
          // The split strip under the plot adds no curve points.
          if ((e.target as Element).closest?.(".curve-split, .curve-split-track")) return;
          const p = position(e);
          if (!p) return;
          const nearest = points.findIndex(
            (q) =>
              Math.hypot(q.x - p.x, q.y - p.y) < 14 || Math.abs(q.x - p.x) < 1,
          );
          if (nearest === -1 && points.length >= MAX_CURVE_POINTS) return;
          const next =
            nearest === -1 ? [...points, p].sort((a, b) => a.x - b.x) : points;
          setSelected(nearest === -1 ? next.indexOf(p) : nearest);
          if (nearest === -1) onChange({ ...curves, [channel]: next });
          dragging.current = true;
          onInteraction(true);
          e.currentTarget.setPointerCapture(e.pointerId);
        }}
        onPointerMove={(e) => {
          if (disabled || !dragging.current || !e.buttons || selected === null)
            return;
          const p = position(e);
          if (!p) return;
          const points = latest.current;
          const next = points.map((q, i) =>
            i === selected
              ? {
                  x:
                    i === 0
                      ? 0
                      : i === points.length - 1
                        ? 255
                        : Math.max(
                            points[i - 1].x + 1,
                            Math.min(points[i + 1].x - 1, p.x),
                          ),
                  y: p.y,
                }
              : q,
          );
          latest.current = next;
          setDragged(next);
          send.push(next);
        }}
        onPointerUp={(e) => {
          if (e.currentTarget.hasPointerCapture(e.pointerId))
            e.currentTarget.releasePointerCapture(e.pointerId);
          end();
        }}
        onPointerCancel={end}
        onLostPointerCapture={end}
      >
        {histogramPath && (
          <path d={histogramPath} className="curve-histogram" data-channel={channel === "luminance" ? "luma" : channel} />
        )}
        {[64, 128, 192].map((p) => (
          <g key={p} className="curve-grid">
            <line x1={p} x2={p} y1="0" y2="255" />
            <line y1={p} y2={p} x1="0" x2="255" />
          </g>
        ))}
        <line x1="0" y1="255" x2="255" y2="0" className="curve-reference" />
        {channel === "luma" && region && <RegionCurveLine region={region} />}
        <polyline
          points={drawn}
          fill="none"
          className={`curve-line curve-${channel}`}
        />
        {marker !== null && (
          <g className="curve-target" data-testid="curve-target-marker">
            <line x1={marker} x2={marker} y1="0" y2="255" />
            <circle
              cx={marker}
              cy={255 - pointCurveValue(rendered, marker)}
              r={3.5}
            />
          </g>
        )}
        {dragged && selected !== null && points[selected] && (
          // Capture One's crosshair through the dragged point.
          <g className="curve-crosshair">
            <line x1={points[selected].x} x2={points[selected].x} y1="0" y2="255" />
            <line x1="0" x2="255" y1={255 - points[selected].y} y2={255 - points[selected].y} />
          </g>
        )}
        {splits && (
          <>
            <rect className="curve-split-track" x="0" y={258} width="255" height={SPLIT_STRIP - 3} />
            {SPLIT_KEYS.map((key) => {
              const [min, max] = SPLIT_RANGES[key];
              const value = splitDrag === key && splitLive !== null ? splitLive : splits[key];
              const name = t(SPLIT_LABELS[key]);
              return (
                <g
                  key={key}
                  role="slider"
                  tabIndex={disabled ? -1 : 0}
                  aria-label={name}
                  aria-valuemin={min}
                  aria-valuemax={max}
                  aria-valuenow={value}
                  aria-disabled={disabled || undefined}
                  className={`curve-split ${splitDrag === key ? "is-dragging" : ""}`}
                  transform={`translate(${(value * 255) / 100} 0)`}
                  onPointerDown={(e) => {
                    if (disabled || e.button !== 0) return;
                    e.stopPropagation();
                    e.currentTarget.setPointerCapture(e.pointerId);
                    splitActive.current = true;
                    setSplitDrag(key);
                    setSplitLive(splits[key]);
                    onInteraction(true);
                  }}
                  onPointerMove={(e) => {
                    if (splitDrag !== key || !e.buttons) return;
                    const matrix = svgRef.current?.getScreenCTM();
                    if (!matrix) return;
                    const { x } = curvePointerPosition(e.clientX, e.clientY, matrix.inverse());
                    setSplit(key, (x * 100) / 255);
                  }}
                  onPointerUp={(e) => {
                    if (splitDrag !== key) return;
                    e.stopPropagation();
                    if (e.currentTarget.hasPointerCapture(e.pointerId))
                      e.currentTarget.releasePointerCapture(e.pointerId);
                    endSplit();
                  }}
                  onPointerCancel={() => splitDrag === key && endSplit()}
                  onLostPointerCapture={() => splitDrag === key && endSplit()}
                  onKeyDown={(e) => {
                    if (disabled) return;
                    const step = e.shiftKey ? 10 : 1;
                    const next =
                      e.key === "ArrowLeft" || e.key === "ArrowDown"
                        ? value - step
                        : e.key === "ArrowRight" || e.key === "ArrowUp"
                          ? value + step
                          : e.key === "Home"
                            ? min
                            : e.key === "End"
                              ? max
                              : null;
                    if (next === null) return;
                    e.preventDefault();
                    e.stopPropagation();
                    debouncedSetHistory.flush();
                    setSplit(key, next);
                    debouncedSetHistory.flush();
                  }}
                  onDoubleClick={(e) => {
                    e.stopPropagation();
                    if (!disabled) setSplitEdit(key);
                  }}
                >
                  <title>{t("studio.tone.regionCurve.splitHint", { name, value })}</title>
                  <path d={`M0 ${259} L-5 ${255 + SPLIT_STRIP} L5 ${255 + SPLIT_STRIP} Z`} />
                  <text className="curve-split-value" x="0" y="251" textAnchor="middle">
                    {value}
                  </text>
                </g>
              );
            })}
          </>
        )}
        {points.map((p, i) => (
          <circle
            key={i}
            cx={p.x}
            cy={255 - p.y}
            r={selected === i ? 4.5 : 3}
            className="curve-point"
            data-curve-point={i}
            tabIndex={disabled ? -1 : 0}
            role="button"
            aria-label={t("studio.develop.curve.point", {
              index: i + 1,
              input: Math.round(p.x),
              output: Math.round(p.y),
            })}
            onFocus={() => setSelected(i)}
            onKeyDown={(e) => {
              if (disabled || e.metaKey || e.ctrlKey || e.altKey) return;
              if (e.key === "Delete" || e.key === "Backspace") {
                e.preventDefault();
                if (i === 0 || i === points.length - 1) return;
                keyboardChange(points.filter((_, n) => n !== i));
                setSelected(i - 1);
                focusPoint(i - 1);
              } else if (e.key.toLowerCase() === "a") {
                e.preventDefault();
                if (points.length >= MAX_CURVE_POINTS) return;
                const left = i === points.length - 1 ? i - 1 : i;
                const x = (points[left].x + points[left + 1].x) / 2;
                if (x - points[left].x < 1 || points[left + 1].x - x < 1)
                  return;
                const curve =
                  channel === "luminance"
                    ? renderedLuminancePoints(points)
                    : renderedPoints(points, wholeCurves);
                const next = [...points];
                next.splice(left + 1, 0, { x, y: pointCurveValue(curve, x) });
                keyboardChange(next);
                setSelected(left + 1);
                focusPoint(left + 1);
              } else if (
                ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(
                  e.key,
                )
              ) {
                e.preventDefault();
                const dx =
                  e.key === "ArrowLeft" ? -1 : e.key === "ArrowRight" ? 1 : 0;
                const dy =
                  e.key === "ArrowDown" ? -1 : e.key === "ArrowUp" ? 1 : 0;
                const x =
                  i === 0 || i === points.length - 1
                    ? p.x
                    : Math.max(
                        points[i - 1].x + 1,
                        Math.min(points[i + 1].x - 1, p.x + dx),
                      );
                const y = Math.max(0, Math.min(255, p.y + dy));
                if (x !== p.x || y !== p.y)
                  keyboardChange(points.map((q, n) => (n === i ? { x, y } : q)));
              }
            }}
            onDoubleClick={(e) => {
              e.stopPropagation();
              if (!disabled && i > 0 && i < points.length - 1) {
                onChange({
                  ...curves,
                  [channel]: points.filter((_, n) => n !== i),
                });
                setSelected(null);
              }
            }}
          />
        ))}
      </svg>
      {splits && splitEdit && (
        <label className="curve-split-entry">
          {t(SPLIT_LABELS[splitEdit])}
          <input
            type="number"
            aria-label={t(SPLIT_LABELS[splitEdit])}
            min={SPLIT_RANGES[splitEdit][0]}
            max={SPLIT_RANGES[splitEdit][1]}
            defaultValue={splits[splitEdit]}
            autoFocus
            onFocus={(e) => e.currentTarget.select()}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") {
                const value = Number(e.currentTarget.value);
                if (e.currentTarget.value.trim() && Number.isFinite(value)) setSplit(splitEdit, value);
                setSplitEdit(null);
              } else if (e.key === "Escape") {
                setSplitEdit(null);
              }
            }}
            onBlur={(e) => {
              const value = Number(e.currentTarget.value);
              if (e.currentTarget.value.trim() && Number.isFinite(value)) setSplit(splitEdit, value);
              setSplitEdit(null);
            }}
          />
        </label>
      )}
      <span className="sr-only" aria-live="polite" aria-atomic="true">
        {selected !== null && points[selected] && t("studio.develop.curve.point", {
          index: selected + 1,
          input: Math.round(points[selected].x),
          output: Math.round(points[selected].y),
        })}
      </span>
      {selected !== null && points[selected] && (
        <div className="curve-coordinate">
          <label>
            {t("studio.develop.curve.input")}{" "}
            <input
              {...selectOnFocus}
              type="number"
              min="0"
              max="255"
              value={Math.round(points[selected].x)}
              disabled={
                disabled || selected === 0 || selected === points.length - 1
              }
              onChange={(e) => {
                const value = Number(e.target.value);
                onChange({
                  ...curves,
                  [channel]: points.map((p, i) =>
                    i === selected
                      ? {
                          ...p,
                          x: Math.max(
                            points[i - 1].x + 1,
                            Math.min(points[i + 1].x - 1, value),
                          ),
                        }
                      : p,
                  ),
                });
              }}
            />
          </label>
          <label>
            {t("studio.develop.curve.output")}{" "}
            <input
              {...selectOnFocus}
              type="number"
              min="0"
              max="255"
              value={Math.round(points[selected].y)}
              disabled={disabled}
              onChange={(e) =>
                onChange({
                  ...curves,
                  [channel]: points.map((p, i) =>
                    i === selected
                      ? {
                          ...p,
                          y: Math.max(0, Math.min(255, Number(e.target.value))),
                        }
                      : p,
                  ),
                })
              }
            />
          </label>
        </div>
      )}
      {points.length >= MAX_CURVE_POINTS && (
        <p className="studio-help">
          {t("studio.develop.curve.maxPoints", { max: MAX_CURVE_POINTS })}
        </p>
      )}
    </div>
  );
}

export default function StudioDevelop() {
  const { t } = useTranslation();
  const appSettings = useSettingsStore((s) => s.appSettings);
  const handleSettingsChange = useSettingsStore((s) => s.handleSettingsChange);
  const setAppSettings = useSettingsStore((s) => s.setAppSettings);
  const isMac = useSettingsStore((s) => s.osPlatform === "macos");
  const maskDisplay = maskDisplayOf(appSettings);
  const maskDisplayLabel = t("studio.develop.maskOverlay.currentMode", {
    mode: t(`studio.develop.maskOverlay.modes.${maskDisplay}`),
  });
  const maskColor = maskColorOf(appSettings);
  // Names of the overlay swatches (MASK_COLOR_PRESETS keeps English ids).
  const maskColorName = (name: string) =>
    ({
      Red: t("studio.develop.maskOverlay.colors.red"),
      Green: t("studio.develop.maskOverlay.colors.green"),
      White: t("studio.develop.maskOverlay.colors.white"),
      Black: t("studio.develop.maskOverlay.colors.black"),
    })[name] ?? name;
  const maskOpacity = maskOpacityOf(appSettings);
  // Overlay colour/opacity apply at once while dragging; the settings file is
  // written once the value settles instead of on every frame.
  const settingsSave = useRef<number | null>(null);
  const saveSettingsNow = () => {
    if (settingsSave.current === null) return;
    window.clearTimeout(settingsSave.current);
    settingsSave.current = null;
    const latest = useSettingsStore.getState().appSettings;
    if (latest) void useSettingsStore.getState().handleSettingsChange(latest);
  };
  const saveSettingsNowRef = useRef(saveSettingsNow);
  saveSettingsNowRef.current = saveSettingsNow;
  useEffect(() => () => saveSettingsNowRef.current(), []);
  const previewSettings = (patch: Partial<AppSettings>) => {
    const current = useSettingsStore.getState().appSettings;
    if (!current) return;
    setAppSettings({ ...current, ...patch });
    if (settingsSave.current !== null)
      window.clearTimeout(settingsSave.current);
    settingsSave.current = window.setTimeout(saveSettingsNow, 300);
  };
  const {
    adjustments,
    selectedImage,
    activeMaskContainerId,
    isWbPickerActive,
    aiDenoiseBusy,
    setEditor,
  } = useEditorStore(
    useShallow((s) => ({
      adjustments: s.adjustments,
      selectedImage: s.selectedImage,
      activeMaskContainerId: s.activeMaskContainerId,
      isWbPickerActive: s.isWbPickerActive,
      aiDenoiseBusy: s.aiDenoiseBusy,
      setEditor: s.setEditor,
    })),
  );
  // The next photograph loading after a ready one: the controls keep their
  // look (the tools are inert meanwhile) and take its values in place.
  const panelsReady = useEditorStore(panelsShowReady);
  const { setAdjustments, handleAutoAdjustments, handleAutoWhiteBalance } = useEditorActions();
  const activePanel = useUIStore((s) => s.activePanel);
  const [tab, setTab] = useState("all");
  useEffect(() => {
    if (activePanel === Panel.Crop) setTab("geometry");
    // Capture One keeps Layers on top of the Adjustments tab: selecting a
    // layer there keeps the tab and the tools act on the layer.
    else if (activePanel === Panel.Masks)
      setTab((current) => (current === "all" ? "all" : "layers"));
    else if (activePanel === Panel.Adjustments)
      setTab((current) =>
        current === "geometry" || current === "layers" ? "all" : current,
      );
  }, [activePanel]);
  // Lightroom opens the Remove tool's panel with the tool: the spot tool
  // opens the Retouch tab, which holds its settings, and ending it returns
  // to the tab it was started from unless another tab was chosen meanwhile.
  // The tool ends cropping and layer editing, so from Crop or Layers it
  // returns to Adjust, as the panel change above does.
  const isSpotToolActive = useEditorStore((s) => s.isSpotToolActive);
  const tabRef = useRef(tab);
  tabRef.current = tab;
  const tabBeforeSpot = useRef<string | null>(null);
  useEffect(() => {
    if (isSpotToolActive) {
      const current = tabRef.current;
      if (current !== "retouch")
        tabBeforeSpot.current = current === "geometry" || current === "layers" ? "all" : current;
      setTab("retouch");
      return;
    }
    const before = tabBeforeSpot.current;
    tabBeforeSpot.current = null;
    if (before) setTab((current) => (current === "retouch" ? before : current));
  }, [isSpotToolActive]);
  // A layer's mask overlay belongs to the tabs that edit masks (Adjust,
  // Layers), as Lightroom shows it only in the Masking panel: elsewhere it
  // shows only while a mask is drawn.
  const maskTab = tab === "all" || tab === "layers";
  useEffect(() => {
    setEditor({ maskOverlayOutsideMaskTab: !maskTab });
  }, [maskTab, setEditor]);
  useEffect(() => () => setEditor({ maskOverlayOutsideMaskTab: false }), [setEditor]);
  const [hsl, setHsl] = useState("reds");
  // Targeted Adjustment Tool (Lightroom's TAT): one target at a time; the
  // colour mixer follows the channel a drag acts on most.
  const { tatTarget, tatFocus } = useEditorStore(
    useShallow((s) => ({ tatTarget: s.tatTarget, tatFocus: s.tatFocus })),
  );
  useEffect(() => {
    if (tatFocus) setHsl(tatFocus);
  }, [tatFocus]);
  const targetToggle = (target: TatTarget, label: string, text?: string) => (
    <button
      type="button"
      className={text ? "studio-tat-button" : undefined}
      aria-label={label}
      aria-pressed={tatTarget === target}
      title={t("studio.develop.targeted.hint", { tool: label })}
      disabled={!panelsReady}
      onClick={(e) => {
        e.preventDefault();
        setEditor({
          tatTarget: tatTarget === target ? null : target,
          isWbPickerActive: false,
          colorPickerTarget: null,
        });
      }}
    >
      {text ?? <Crosshair size={12} />}
    </button>
  );
  // Overlay display settings stay folded away until asked for.
  const [maskOptions, setMaskOptions] = useState(false);
  const layer =
    activePanel === Panel.Masks
      ? adjustments.masks.find((m) => m.id === activeMaskContainerId)
      : undefined;
  const values = layer?.adjustments || adjustments;
  const onInteraction = useCallback(
    (active: boolean) =>
      useEditorStore.getState().setEditor({ isSliderDragging: active }),
    [],
  );
  const change = (patch: Partial<Adjustments>) =>
    selectedImage?.isReady &&
    setAdjustments((prev) =>
      layer
        ? {
            ...prev,
            masks: prev.masks.map((m) =>
              m.id === layer.id
                ? { ...m, adjustments: { ...m.adjustments, ...patch } }
                : m,
            ),
          }
        : withNewGrainStructure(prev, { ...prev, ...patch }),
    );
  // One callback per value for the sliders, calling this render's `change`:
  // the memoized controls then skip re-rendering while another slider moves.
  const latestChange = useRef(change);
  latestChange.current = change;
  const keyedChanges = useRef(new Map<string, (value: number) => void>());
  const changeOf = (key: string) => {
    let onChange = keyedChanges.current.get(key);
    if (!onChange) {
      onChange = (value: number) => latestChange.current({ [key]: value });
      keyedChanges.current.set(key, onChange);
    }
    return onChange;
  };
  // Layers start neutral; the photo itself starts from the RAW defaults.
  const defaults = layer
    ? INITIAL_ADJUSTMENTS
    : initialAdjustmentsFor(!!selectedImage?.isRaw);
  const reset = (keys: string[]) =>
    change(
      Object.fromEntries(
        keys.map((key) => [key, structuredClone(defaults[key])]),
      ),
    );
  const control = (
    label: string,
    key: string,
    min = -100,
    max = 100,
    step = 1,
    track?: "temperature" | "tint" | "hue",
  ) => (
    <AdjustmentControl
      key={key}
      label={label}
      value={(values[key] ?? defaults[key] ?? 0) as number}
      min={min}
      max={max}
      step={step}
      defaultValue={(defaults[key] ?? 0) as number}
      disabled={!panelsReady}
      track={track}
      onChange={changeOf(key)}
      onInteraction={onInteraction}
    />
  );
  // Relative Temperature/Tint: JPEGs, layers and RAW edits made before the
  // Kelvin white balance (shown while they hold a correction).
  // The picker sits in the WB row beside the presets (Lightroom and Capture
  // One); without absolute white balance (layers, files without camera
  // calibration) it heads the relative sliders.
  const wbPicker = (
    <button
      type="button"
      aria-label={t("studio.develop.whiteBalance.picker")}
      aria-pressed={isWbPickerActive}
      disabled={!panelsReady || !!layer}
      title={
        layer
          ? t("studio.develop.whiteBalance.pickerOnBackground")
          : t("studio.develop.whiteBalance.pickNeutral")
      }
      onClick={() => setEditor({ isWbPickerActive: !isWbPickerActive })}
    >
      <Pipette size={15} />
    </button>
  );
  const relativePickerRow = (
    <div className="studio-inline">
      <span>{t("studio.develop.whiteBalance.relative")}</span>
      {wbPicker}
    </div>
  );
  const relativeWhiteBalance = (
    <>
      {control(
        t("adjustments.color.temperature"),
        "temperature",
        -100,
        100,
        1,
        "temperature",
      )}
      {control(t("adjustments.color.tint"), "tint", -100, 100, 1, "tint")}
    </>
  );
  // A refinement under its amount (Lightroom's Detail, Contrast,
  // Smoothness): 0–100 with its own default, idle while the amount is 0.
  const refinement = (
    label: string,
    key: string,
    fallback: number,
    ariaLabel: string,
    idle: boolean,
  ) => (
    <AdjustmentControl
      key={key}
      label={label}
      ariaLabel={ariaLabel}
      value={(values[key] ?? fallback) as number}
      min={0}
      max={100}
      step={1}
      defaultValue={fallback}
      disabled={!panelsReady || idle}
      onChange={changeOf(key)}
      onInteraction={onInteraction}
    />
  );
  // A tab chooses tools, not what they act on: a selected layer stays the
  // target in every tab but Crop (as Capture One's layer selector does);
  // tools the layer cannot take are hidden there.
  const selectTab = (next: string) => {
    tabBeforeSpot.current = null;
    setTab(next);
    const keepLayer = !!layer && next !== "geometry";
    useUIStore
      .getState()
      .setPanel(
        next === "layers" || keepLayer
          ? Panel.Masks
          : next === "geometry"
            ? Panel.Crop
            : Panel.Adjustments,
      );
    if (next !== "layers" && !keepLayer)
      setEditor({ activeMaskContainerId: null, activeMaskId: null });
    setEditor({ isWbPickerActive: false });
  };
  const editBackground = () => {
    setEditor({ activeMaskContainerId: null, activeMaskId: null });
    useUIStore.getState().setPanel(Panel.Adjustments);
  };
  const inTab = (...names: string[]) => names.includes(tab);
  // Copy/apply works on the photograph's own values, not a layer's.
  const photoOnly = (params: ToolParams) => (layer ? undefined : params);
  // Per-tool switch in sectionVisibility (missing = on).
  const toolSwitch = (section: string) => ({
    enabled: values.sectionVisibility?.[section] !== false,
    onToggle: () =>
      change({
        sectionVisibility: {
          ...values.sectionVisibility,
          [section]: values.sectionVisibility?.[section] === false,
        },
      }),
  });
  // Before Exposure for the photograph, after it for a layer.
  const whiteBalanceGroup = inTab("all", "layers") && (
            <ToolGroup
              key="whiteBalance"
              title="White balance"
              label={t("studio.develop.groups.whiteBalance")}
              onReset={() =>
                reset(
                  layer
                    ? ["temperature", "tint"]
                    : [
                        "temperature",
                        "tint",
                        "whiteBalance",
                        "wbTemperature",
                        "wbTint",
                      ],
                )
              }
              copy={photoOnly([
                ["whiteBalance", t("studio.develop.groups.whiteBalance")],
                ["wbTemperature", t("studio.develop.whiteBalance.temp")],
                ["wbTint", t("adjustments.color.tint")],
                [
                  "temperature",
                  t("studio.develop.whiteBalance.relativeTemperature"),
                ],
                ["tint", t("studio.develop.whiteBalance.relativeTint")],
              ])}
              toggle={toolSwitch("whiteBalance")}
              actions={
                !layer && (
                  <button
                    type="button"
                    className="studio-tool-auto"
                    disabled={!panelsReady}
                    aria-label={t("studio.develop.whiteBalance.auto")}
                    title={t("studio.develop.whiteBalance.autoTitle", { shortcut: "⌘⇧U" })}
                    onClick={(e) => {
                      e.preventDefault();
                      void handleAutoWhiteBalance();
                    }}
                  >
                    <Sparkles size={12} />
                    {t("studio.develop.auto.label")}
                  </button>
                )
              }
            >
              {selectedImage?.isRaw && !layer ? (
                <StudioWhiteBalance
                  values={adjustments}
                  imagePath={selectedImage.path}
                  disabled={!panelsReady}
                  onChange={change}
                  onAuto={() => void handleAutoWhiteBalance()}
                  onInteraction={onInteraction}
                  picker={wbPicker}
                  fallback={
                    <>
                      {relativePickerRow}
                      {relativeWhiteBalance}
                    </>
                  }
                />
              ) : (
                <>
                  {relativePickerRow}
                  {relativeWhiteBalance}
                </>
              )}
              {selectedImage?.isRaw &&
                !layer &&
                (!!adjustments.temperature || !!adjustments.tint) &&
                relativeWhiteBalance}
            </ToolGroup>
  );

  return (
    <div className="studio-develop">
      <PerfProfiler id="histogram">
        <StudioHistogram />
      </PerfProfiler>
      <nav
        className="studio-tool-tabs"
        aria-label={t("studio.develop.tabs.aria")}
      >
        {[
          [
            "all",
            t("studio.develop.tabs.adjust"),
            t("studio.develop.tabs.adjustTitle"),
            SlidersHorizontal,
          ],
          [
            "color",
            t("studio.develop.tabs.color"),
            t("studio.develop.tabs.color"),
            Palette,
          ],
          [
            "detail",
            t("studio.develop.tabs.detail"),
            t("studio.develop.tabs.detail"),
            Focus,
          ],
          [
            "geometry",
            t("studio.develop.tabs.crop"),
            t("studio.develop.tabs.cropTitle"),
            Crop,
          ],
          [
            "retouch",
            t("studio.develop.tabs.retouch"),
            t("studio.develop.tabs.retouch"),
            ScanFace,
          ],
          [
            "layers",
            t("studio.develop.tabs.layers"),
            t("studio.develop.tabs.layersTitle"),
            Layers,
          ],
        ].map(([id, label, title, Icon]) => {
          const ToolIcon = Icon as typeof Focus;
          return (
            <button
              key={id as string}
              title={title as string}
              aria-label={title as string}
              aria-pressed={tab === id}
              onClick={() => selectTab(id as string)}
            >
              <ToolIcon size={16} />
              <span>{label as string}</span>
            </button>
          );
        })}
      </nav>
      <div
        className="studio-tools-scroll"
        inert={Boolean(selectedImage && !selectedImage.isReady)}
      >
        {selectedImage && !layer && inTab("all", "detail") && (
          <div className="studio-effects-master">
            <button
              aria-label={t("studio.develop.groups.effects")}
              aria-pressed={values.sectionVisibility?.effects !== false}
              disabled={!panelsReady}
              title={t(values.sectionVisibility?.effects !== false
                ? "studio.develop.toolGroup.turnOff" : "studio.develop.toolGroup.turnOn",
                { tool: t("studio.develop.groups.effects") })}
              onClick={toolSwitch("effects").onToggle}
            >
              {values.sectionVisibility?.effects !== false ? <Eye size={12} /> : <EyeOff size={12} />}
              {t("studio.develop.groups.effects")}
            </button>
            <span>{[t("adjustments.effects.vignette"), t("adjustments.effects.grain"),
              t("studio.develop.groups.lightEffects"), t("adjustments.effects.lut")].join(" · ")}</span>
          </div>
        )}
        {!selectedImage && (
          <p className="studio-help">
            {t("studio.develop.noPhoto")}
          </p>
        )}
        {selectedImage &&
          // Sections without their own eye in a tool header (inherited
          // master switches); the others show their state in the header.
          (
            [
              ["color", t("studio.develop.tabs.color")],
              ["details", t("studio.develop.tabs.detail")],
            ] as const
          )
            .filter(([key]) => values.sectionVisibility?.[key] === false)
            .map(([key, label]) => (
              <div className="studio-inline" key={key}>
                <span>
                  {t("studio.develop.bypass.bypassed", { section: label })}
                </span>
                <button
                  aria-label={t("studio.develop.bypass.enableSection", {
                    section: label,
                  })}
                  onClick={() =>
                    change({
                      sectionVisibility: {
                        ...values.sectionVisibility,
                        [key]: true,
                      },
                    })
                  }
                >
                  <Eye size={14} /> {t("studio.develop.bypass.enable")}
                </button>
              </div>
            ))}
        {layer && tab !== "all" && tab !== "layers" && (
          <div className="studio-target" role="status">
            <Layers size={13} aria-hidden />
            <span>{t("studio.develop.target.layer", { name: layer.name })}</span>
            <button type="button" onClick={editBackground}>
              {t("studio.develop.target.background")}
            </button>
          </div>
        )}
        {layer && tab === "retouch" && (
          <p className="studio-help">{t("studio.develop.target.photoOnly")}</p>
        )}
        <ToolColumn key={tab} scope={`develop:${tab}`}>
          {(tab === "layers" || tab === "all") && (
            <ToolGroup
              key="layers"
              title="Layers & masks"
              label={t("studio.develop.groups.layersMasks")}
              // In Adjust an empty list starts folded, so White balance and
              // Exposure are in view on a laptop (Lightroom keeps masking
              // out of the Basic panel); a photo with layers opens it.
              open={tab === "layers" || adjustments.masks.length > 0}
              actions={
                <>
                  <button
                    type="button"
                    className="studio-mask-display-button"
                    aria-label={maskDisplay === "always" ? t("studio.develop.maskOverlay.button") : maskDisplayLabel}
                    aria-pressed={maskOptions}
                    title={maskDisplay === "always" ? t("studio.develop.maskOverlay.title") : maskDisplayLabel}
                    onClick={(e) => {
                      e.preventDefault();
                      setMaskOptions(!maskOptions);
                    }}
                  >
                    <Eye size={12} />
                    {maskDisplay !== "always" && <span className="studio-mask-display-dot" aria-hidden="true" />}
                  </button>
                  <NewLayerButton />
                </>
              }
            >
              {maskOptions && (
              <div className="studio-mask-options">
              <div className="studio-inline">
                <label
                  title={t("studio.develop.maskOverlay.showMaskTitle", {
                    key: isMac ? "⌥M" : "Alt+M",
                  })}
                >
                  {t("studio.develop.maskOverlay.showMask")}
                  <select
                    aria-label={t("studio.develop.maskOverlay.display")}
                    value={maskDisplayOf(appSettings)}
                    disabled={!appSettings}
                    onChange={(e) => {
                      if (!appSettings) return;
                      handleSettingsChange({
                        ...appSettings,
                        ...chooseMaskDisplay(
                          appSettings,
                          e.target.value as MaskDisplay,
                        ),
                      });
                      // A mode chosen here shows the overlay, also after the
                      // layer's first adjustment switched it off.
                      setEditor({ maskOverlayAuto: "pinned" });
                    }}
                  >
                    {MASK_DISPLAY_OPTIONS.map(([mode]) => (
                      <option key={mode} value={mode}>
                        {t(`studio.develop.maskOverlay.modes.${mode}`)}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
              <div className="studio-inline">
                <span>{t("studio.develop.maskOverlay.color")}</span>
                <div
                  className="studio-mask-colors"
                  role="group"
                  aria-label={t("studio.develop.maskOverlay.color")}
                >
                  {MASK_COLOR_PRESETS.map(([color, name]) => (
                    <button
                      key={color}
                      type="button"
                      className="studio-mask-swatch"
                      style={{ background: color }}
                      title={maskColorName(name)}
                      aria-label={t("studio.develop.maskOverlay.colorMask", {
                        color: maskColorName(name),
                      })}
                      aria-pressed={maskColor === color}
                      disabled={!appSettings}
                      onClick={() => previewSettings({ maskColor: color })}
                    />
                  ))}
                  <input
                    type="color"
                    className="studio-mask-picker"
                    title={t("studio.develop.maskOverlay.custom")}
                    aria-label={t("studio.develop.maskOverlay.custom")}
                    value={maskColor}
                    disabled={!appSettings}
                    onChange={(e) =>
                      previewSettings({ maskColor: e.target.value })
                    }
                  />
                </div>
              </div>
              <AdjustmentControl
                label={t("studio.develop.maskOverlay.opacity")}
                min={0}
                max={100}
                unit="%"
                value={maskOpacity}
                defaultValue={DEFAULT_MASK_OPACITY}
                disabled={!appSettings}
                onChange={(maskOpacity) => previewSettings({ maskOpacity })}
              />
              </div>
              )}
              <StudioLayers compact={tab === "all"} />
            </ToolGroup>
          )}
          {tab === "all" && selectedImage && <StudioPresets key="presets" />}
          {inTab("all") && selectedImage?.isRaw && !layer && (
            <ToolGroup
              key="baseCharacteristics"
              title="Base characteristics"
              label={t("studio.develop.groups.baseCharacteristics")}
              copy={photoOnly([
                ["baseCurve", t("studio.develop.baseCurve.curve")],
                ["cameraProfile", t("studio.develop.baseCurve.profile")],
              ])}
              onReset={() =>
                change({ baseCurve: "standard", cameraProfile: undefined })
              }
            >
              <StudioCameraProfile
                value={adjustments.cameraProfile}
                imagePath={selectedImage?.path}
                disabled={!panelsReady}
                onChange={(cameraProfile) => change({ cameraProfile })}
              />
              <label className="studio-inline">
                <span>{t("studio.develop.baseCurve.curve")}</span>
                <select
                  aria-label={t("studio.develop.baseCurve.curve")}
                  value={adjustments.baseCurve ?? "standard"}
                  disabled={!panelsReady}
                  onChange={(e) =>
                    change({
                      baseCurve: e.target.value as Adjustments["baseCurve"],
                    })
                  }
                >
                  <option value="standard">
                    {t("studio.develop.baseCurve.standard")}
                  </option>
                  <option value="highContrast">
                    {t("studio.develop.baseCurve.highContrast")}
                  </option>
                  <option value="extraShadow">
                    {t("studio.develop.baseCurve.extraShadow")}
                  </option>
                  <option value="linear">
                    {t("studio.develop.baseCurve.linear")}
                  </option>
                </select>
              </label>
            </ToolGroup>
          )}
          {!layer && whiteBalanceGroup}
          {inTab("all", "layers") && (
            <ToolGroup
              key="exposure"
              title="Exposure"
              label={t("studio.develop.groups.exposure")}
              toggle={toolSwitch("basic")}
              actions={
                !layer && (
                  <button
                    type="button"
                    className="studio-tool-auto"
                    disabled={!panelsReady}
                    title={t("studio.develop.auto.title", { shortcut: "⌘L" })}
                    onClick={(e) => {
                      e.preventDefault();
                      handleAutoAdjustments();
                    }}
                  >
                    <Sparkles size={12} />
                    {t("studio.develop.auto.label")}
                  </button>
                )
              }
              copy={photoOnly([
                ["exposure", t("studio.develop.sliders.exposure")],
                ["contrast", t("studio.develop.sliders.contrast")],
                ["highlights", t("studio.develop.sliders.highlights")],
                ["shadows", t("studio.develop.sliders.shadows")],
                ["whites", t("studio.develop.sliders.whites")],
                ["blacks", t("studio.develop.sliders.blacks")],
                ["brightness", t("studio.develop.sliders.brightness")],
              ])}
              onReset={() =>
                reset([
                  "exposure",
                  "contrast",
                  "brightness",
                  "highlights",
                  "shadows",
                  "whites",
                  "blacks",
                  // The photo's tone-slider response goes back to a new edit's.
                  ...(layer ? [] : ["toneVersion"]),
                ])
              }
            >
              {control(
                t("studio.develop.sliders.exposure"),
                "exposure",
                -5,
                5,
                0.01,
              )}
              {control(t("studio.develop.sliders.contrast"), "contrast")}
              {control(t("studio.develop.sliders.highlights"), "highlights")}
              {control(t("studio.develop.sliders.shadows"), "shadows")}
              {control(t("studio.develop.sliders.whites"), "whites")}
              {control(t("studio.develop.sliders.blacks"), "blacks")}
              {control(
                t("studio.develop.sliders.brightness"),
                "brightness",
                BRIGHTNESS_CONTROL.min,
                BRIGHTNESS_CONTROL.max,
                BRIGHTNESS_CONTROL.step,
              )}
            </ToolGroup>
          )}
          {/* For a layer Exposure comes first (dodge and burn, Lightroom's
              masks); White balance follows it. */}
          {layer && whiteBalanceGroup}
          {inTab("all", "layers") && (
            <ToolGroup
              key="presence"
              title="Presence"
              label={t("studio.develop.groups.presence")}
              toggle={toolSwitch("presence")}
              copy={photoOnly([
                ["clarity", t("adjustments.details.clarity")],
                ["structure", t("adjustments.details.structure")],
                ["dehaze", t("adjustments.details.dehaze")],
                ["vibrance", t("adjustments.color.vibrance")],
                ["saturation", t("adjustments.color.saturation")],
              ])}
              onReset={() =>
                reset([
                  "clarity",
                  "structure",
                  "dehaze",
                  "vibrance",
                  "saturation",
                  // The photo's Clarity response goes back to a new edit's.
                  ...(layer ? ["hue"] : ["clarityVersion"]),
                ])
              }
            >
              {control(t("adjustments.details.clarity"), "clarity")}
              {control(t("adjustments.details.structure"), "structure")}
              {control(t("adjustments.details.dehaze"), "dehaze")}
              {control(t("adjustments.color.vibrance"), "vibrance")}
              {control(t("adjustments.color.saturation"), "saturation")}
              {/* Lightroom's local Hue: turns the layer's colours (Oklab LCh,
                  lightness and chroma kept). */}
              {layer &&
                control(t("adjustments.color.hue"), "hue", -180, 180, 1, "hue")}
            </ToolGroup>
          )}
          {inTab("all") && (
            <ToolGroup
              key="toneCurve"
              title="Tone curve"
              label={t("studio.develop.groups.toneCurve")}
              toggle={toolSwitch("curves")}
              actions={
                !layer &&
                targetToggle("curve", t("studio.develop.targeted.toneCurve"))
              }
              copy={photoOnly([
                ["curves", t("studio.develop.params.curves")],
                ["regionCurve", t("studio.develop.params.regionCurve")],
              ])}
              open={false}
              onReset={() =>
                change({
                  curves: structuredClone(INITIAL_ADJUSTMENTS.curves),
                  pointCurves: structuredClone(INITIAL_ADJUSTMENTS.curves),
                  parametricCurve: structuredClone(
                    INITIAL_ADJUSTMENTS.parametricCurve,
                  ),
                  curveMode: "point",
                  ...(layer ? {} : { regionCurve: { ...INITIAL_REGION_CURVE } }),
                })
              }
            >
              <PointCurve
                curves={values.curves}
                wholeCurves={
                  !layer &&
                  (values.curves.version ?? 1) >= LONG_CURVE_VERSION
                }
                targeted={!layer}
                region={
                  layer
                    ? undefined
                    : {
                        ...INITIAL_REGION_CURVE,
                        ...(values.regionCurve as RegionCurve | undefined),
                      }
                }
                onRegionChange={
                  layer ? undefined : (regionCurve) => change({ regionCurve })
                }
                disabled={!panelsReady}
                onInteraction={onInteraction}
                onChange={(curves) =>
                  change({
                    // An edit renders the whole curve (curves.version).
                    curves: {
                      ...curves,
                      version: editedCurvesVersion(values.curves),
                    },
                    pointCurves: curves,
                    curveMode: "point",
                  })
                }
              />
              {!layer && (
                <RegionCurveControls
                  targeted={targetToggle(
                    "regionCurve",
                    t("studio.develop.targeted.regionCurve"),
                  )}
                  value={{
                    ...INITIAL_REGION_CURVE,
                    ...(values.regionCurve as RegionCurve | undefined),
                  }}
                  disabled={!panelsReady}
                  onInteraction={onInteraction}
                  onChange={(regionCurve) => change({ regionCurve })}
                />
              )}
            </ToolGroup>
          )}
          {inTab("all") && !layer && (
            <StudioLevels
              key="levels"
              values={values}
              disabled={!panelsReady}
              change={change}
              onInteraction={onInteraction}
              toggle={toolSwitch("levels")}
              copy={photoOnly([["levels", t("studio.develop.params.levels")]])}
            />
          )}
          {inTab("color") && (
            <ToolGroup
              key="colorMixer"
              title="Color mixer"
              label={t("studio.develop.groups.colorMixer")}
              toggle={toolSwitch("colorMixer")}
              copy={photoOnly([["hsl", t("studio.develop.params.hsl")]])}
              open
              onReset={() => reset(["hsl"])}
            >
              {!layer && (
                <div className="studio-inline studio-tat-row">
                  <span>
                    <Crosshair size={12} /> {t("studio.develop.targeted.label")}
                  </span>
                  <span className="studio-tat-group">
                    {targetToggle("hue", t("studio.develop.targeted.hue"), "H")}
                    {targetToggle(
                      "saturation",
                      t("studio.develop.targeted.saturation"),
                      "S",
                    )}
                    {targetToggle(
                      "luminance",
                      t("studio.develop.targeted.luminance"),
                      "L",
                    )}
                  </span>
                </div>
              )}
              <div className="studio-color-channels">
                {(
                  [
                    ["reds", "#dd6767"],
                    ["oranges", "#e6a461"],
                    ["yellows", "#dac878"],
                    ["greens", "#86bb77"],
                    ["aquas", "#72b9b9"],
                    ["blues", "#6d9cc9"],
                    ["purples", "#a18ac5"],
                    ["magentas", "#ca83b0"],
                  ] as const
                ).map(([key, color]) => (
                  <button
                    key={key}
                    aria-label={t(`studio.develop.colorMixer.select.${key}`)}
                    aria-pressed={hsl === key}
                    style={{ "--swatch": color } as React.CSSProperties}
                    onClick={() => setHsl(key)}
                  />
                ))}
              </div>
              {(["hue", "saturation", "luminance"] as const).map((key) => (
                <AdjustmentControl
                  key={key}
                  label={t(`adjustments.color.${key}`)}
                  value={values.hsl[hsl][key]}
                  disabled={!panelsReady}
                  onInteraction={onInteraction}
                  onChange={(v) =>
                    change({
                      hsl: {
                        ...values.hsl,
                        [hsl]: { ...values.hsl[hsl], [key]: v },
                      },
                    })
                  }
                />
              ))}
            </ToolGroup>
          )}
          {inTab("color", "layers") && (
            <StudioColorEditor
              key="colorEditor"
              values={values}
              layerId={layer?.id}
              disabled={!panelsReady}
              change={change}
              onInteraction={onInteraction}
              toggle={toolSwitch("colorEditor")}
              copy={photoOnly([
                ["colorRanges", t("studio.develop.params.colorRanges")],
              ])}
            />
          )}
          {inTab("color", "layers") && (
            <StudioColorGrading
              key="colorGrading"
              values={values}
              disabled={!panelsReady}
              change={change}
              onInteraction={onInteraction}
              toggle={toolSwitch("colorGrading")}
              copy={photoOnly([
                ["colorGrading", t("studio.develop.params.colorGrading")],
              ])}
            />
          )}
          {inTab("color") && !layer && (
            <StudioBlackAndWhite
              key="blackAndWhite"
              targeted={targetToggle(
                "grayMixer",
                t("studio.develop.targeted.bwMix"),
              )}
              values={values}
              disabled={!panelsReady}
              change={change}
              onInteraction={onInteraction}
              copy={photoOnly([
                ["blackAndWhite", t("studio.develop.params.blackAndWhite")],
                ["grayMixer", t("studio.develop.params.bwMix")],
              ])}
            />
          )}
          {inTab("color") && !layer && (
            <StudioCalibration
              key="calibration"
              values={adjustments}
              disabled={!panelsReady}
              change={change}
              onInteraction={onInteraction}
              toggle={toolSwitch("calibration")}
              copy={photoOnly([
                ["colorCalibration", t("studio.develop.params.calibration")],
              ])}
            />
          )}
          {tab === "retouch" && !layer && selectedImage && (
            <ToolGroup
              key="spotRemoval"
              title="Spot removal"
              label={t("studio.tools.bar.spot.label")}
              open
              expand={isSpotToolActive}
              onReset={resetSpotSettings}
            >
              <StudioSpotPanel />
            </ToolGroup>
          )}
          {tab === "retouch" && !layer && (
            // Capture One's Retouch Faces (Skin): per face or all faces.
            // Only in its own tab: showing it finds the photo's faces.
            <ToolGroup
              key="faceRetouch"
              title="Face retouch"
              label={t("studio.develop.groups.faceRetouch")}
              open
              onReset={() => change({ faceRetouch: null })}
            >
              <StudioFaceRetouch onInteraction={onInteraction} />
            </ToolGroup>
          )}
          {inTab("detail") && (
            <ToolGroup
              key="sharpening"
              title="Sharpening"
              label={t("adjustments.details.sharpening")}
              toggle={toolSwitch("sharpening")}
              open
              copy={photoOnly([
                ["sharpness", t("adjustments.effects.amount")],
                ["sharpenRadius", t("studio.develop.sliders.radius")],
                ["sharpenDetail", t("studio.develop.sliders.detail")],
                ["sharpenMasking", t("studio.develop.sliders.masking")],
                ["sharpnessThreshold", t("adjustments.details.threshold")],
                ["sharpnessFalloff", t("studio.develop.sliders.falloff")],
              ])}
              onReset={() =>
                reset(
                  layer
                    ? ["sharpness", "sharpnessThreshold"]
                    : [
                        "sharpness",
                        "sharpenRadius",
                        "sharpenDetail",
                        "sharpenMasking",
                        "sharpnessThreshold",
                        "sharpnessFalloff",
                        // The photo's sharpening response goes back to a new edit's.
                        "sharpenVersion",
                      ],
                )
              }
            >
              {/* Lightroom's range (to 150) from response 2 on, which is
                  measured against Camera Raw up to 150; response 1's halos
                  grew with the amount. Layers add -100 to 100. */}
              {control(
                t("adjustments.effects.amount"),
                "sharpness",
                -100,
                !layer && ((values.sharpenVersion as number | undefined) ?? 1) >= 2 ? 150 : 100,
              )}
              {/* Lightroom's Radius / Detail / Masking act on the whole photo. */}
              {!layer &&
                control(
                  t("studio.develop.sliders.radius"),
                  "sharpenRadius",
                  0.5,
                  3,
                  0.1,
                )}
              {!layer &&
                control(t("studio.develop.sliders.detail"), "sharpenDetail", 0, 100)}
              {!layer &&
                control(
                  t("studio.develop.sliders.masking"),
                  "sharpenMasking",
                  0,
                  100,
                )}
              {control(
                t("adjustments.details.threshold"),
                "sharpnessThreshold",
                0,
                80,
              )}
              {/* Capture One's Sharpness Falloff, for softer lens corners. */}
              {!layer &&
                control(
                  t("studio.develop.sliders.falloff"),
                  "sharpnessFalloff",
                  0,
                  100,
                )}
            </ToolGroup>
          )}
          {inTab("detail") && (
            <ToolGroup
              key="noiseReduction"
              title="Noise reduction"
              label={t("studio.develop.groups.noiseReduction")}
              toggle={toolSwitch("noiseReduction")}
              open
              copy={photoOnly([
                ["aiDenoise", t("studio.develop.sliders.aiDenoise")],
                ["lumaNoiseReduction", t("adjustments.details.luminance")],
                ["lumaNoiseDetail", t("studio.develop.params.lumaDetail")],
                ["lumaNoiseContrast", t("studio.develop.params.lumaContrast")],
                ["colorNoiseReduction", t("adjustments.details.color")],
                ["colorNoiseDetail", t("studio.develop.params.colorDetail")],
                [
                  "colorNoiseSmoothness",
                  t("studio.develop.params.colorSmoothness"),
                ],
              ])}
              onReset={() =>
                reset([
                  "aiDenoise",
                  "lumaNoiseReduction",
                  "colorNoiseReduction",
                  "lumaNoiseDetail",
                  "lumaNoiseContrast",
                  "colorNoiseDetail",
                  "colorNoiseSmoothness",
                  // The photo's colour and luminance noise filters and AI
                  // Denoise version go back to a new edit's.
                  ...(layer ? [] : ["colorNoiseVersion", "lumaNoiseVersion", "aiDenoiseVersion"]),
                ])
              }
            >
              {/* AI Denoise works on the RAW mosaic before demosaicing, so it
                  is photo-wide and develops the photograph again. */}
              {!layer && selectedImage?.isRaw && (
                <>
                  {control(
                    t("studio.develop.sliders.aiDenoise"),
                    "aiDenoise",
                    0,
                    100,
                  )}
                  {aiDenoiseBusy && (
                    <p className="studio-help" role="status">
                      {t("studio.develop.noise.denoising")}
                    </p>
                  )}
                </>
              )}
              {control(
                t("adjustments.details.luminance"),
                "lumaNoiseReduction",
                0,
              )}
              {/* Lightroom's refinements, photo-wide (layers have amounts). */}
              {!layer && (
                <div className="studio-subcontrols">
                  {refinement(
                    t("studio.develop.sliders.detail"),
                    "lumaNoiseDetail",
                    50,
                    t("studio.develop.noise.lumaDetailAria"),
                    !values.lumaNoiseReduction,
                  )}
                  {refinement(
                    t("studio.develop.sliders.contrast"),
                    "lumaNoiseContrast",
                    0,
                    t("studio.develop.noise.lumaContrastAria"),
                    !values.lumaNoiseReduction,
                  )}
                </div>
              )}
              {control(
                t("adjustments.details.color"),
                "colorNoiseReduction",
                0,
              )}
              {!layer && (
                <div className="studio-subcontrols">
                  {refinement(
                    t("studio.develop.sliders.detail"),
                    "colorNoiseDetail",
                    50,
                    t("studio.develop.noise.colorDetailAria"),
                    !values.colorNoiseReduction,
                  )}
                  {refinement(
                    t("studio.develop.sliders.smoothness"),
                    "colorNoiseSmoothness",
                    50,
                    t("studio.develop.noise.colorSmoothnessAria"),
                    !values.colorNoiseReduction,
                  )}
                </div>
              )}
            </ToolGroup>
          )}
          {inTab("detail") && !layer && (
            <ToolGroup
              key="grain"
              title="Grain"
              label={t("adjustments.effects.grain")}
              open={false}
              copy={photoOnly([
                ["grainAmount", t("adjustments.effects.grain")],
                ["grainSize", t("studio.develop.params.grainSize")],
                ["grainRoughness", t("adjustments.effects.roughness")],
                ["grainStructure", t("studio.develop.effects.grainStructure")],
                ["grainColor", t("studio.develop.effects.grainColor")],
              ])}
              onReset={() => reset(["grainAmount", "grainSize", "grainRoughness", "grainStructure", "grainColor", "grainVersion"])}
            >
              {control(t("adjustments.effects.grain"), "grainAmount", 0)}
              {control(t("adjustments.effects.size"), "grainSize", 0)}
              {control(t("adjustments.effects.roughness"), "grainRoughness", 0)}
              {/* Film grain (version 3, notes: reference/grain.md): Colour
                  sets how independent the dye layers' grain is. */}
              {isFilmGrain(values as Partial<Adjustments>) &&
                control(t("studio.develop.effects.grainColor"), "grainColor", 0, 100)}
              {/* Grain structures (notes: reference/grain.md): Even is the
                  original grain; the others are own procedural grains that
                  cover characters Lightroom's sliders cannot reach. */}
              {!isFilmGrain(values as Partial<Adjustments>) && (
              <label className="studio-inline">
                <span>{t("studio.develop.effects.grainStructure")}</span>
                <select
                  aria-label={t("studio.develop.effects.grainStructure")}
                  value={effectiveGrainStructure(values as Partial<Adjustments>)}
                  disabled={!panelsReady}
                  onChange={(e) =>
                    change({
                      grainStructure: e.target.value as GrainStructure,
                    })
                  }
                >
                  <option value="even">
                    {t("studio.develop.effects.grainStructures.even")}
                  </option>
                  <option value="soft">
                    {t("studio.develop.effects.grainStructures.soft")}
                  </option>
                  <option value="clumped">
                    {t("studio.develop.effects.grainStructures.clumped")}
                  </option>
                  <option value="speckled">
                    {t("studio.develop.effects.grainStructures.speckled")}
                  </option>
                  <option value="gritty">
                    {t("studio.develop.effects.grainStructures.gritty")}
                  </option>
                </select>
              </label>
              )}
            </ToolGroup>
          )}
          {inTab("detail") && !layer && (
            <StudioDefringe
              key="defringe"
              values={values}
              disabled={!panelsReady}
              change={change}
              onInteraction={onInteraction}
              copy={photoOnly([
                ["defringePurpleAmount", t("studio.develop.params.purpleAmount")],
                ["defringePurpleHueLo", t("studio.develop.params.purpleHueFrom")],
                ["defringePurpleHueHi", t("studio.develop.params.purpleHueTo")],
                ["defringeGreenAmount", t("studio.develop.params.greenAmount")],
                ["defringeGreenHueLo", t("studio.develop.params.greenHueFrom")],
                ["defringeGreenHueHi", t("studio.develop.params.greenHueTo")],
              ])}
            />
          )}
          {inTab("detail") && layer && (
            // Lightroom's local Defringe: above 0 colour fringes along edges
            // lose their colour (any hue; a coloured object keeps its edge),
            // below 0 the photo's Defringe is held back here.
            <ToolGroup
              key="layerDefringe"
              title="Defringe"
              label={t("studio.develop.groups.defringe")}
              onReset={() => reset(["defringe"])}
            >
              {control(t("studio.develop.groups.defringe"), "defringe")}
            </ToolGroup>
          )}
          {inTab("detail") && (
            <ToolGroup
              key="moire"
              title="Moiré"
              label={t("studio.develop.groups.moire")}
              open={false}
              copy={photoOnly([
                ["moireAmount", t("adjustments.effects.amount")],
                ["moirePattern", t("studio.develop.sliders.pattern")],
              ])}
              onReset={() => reset(["moireAmount", "moirePattern"])}
            >
              {control(t("adjustments.effects.amount"), "moireAmount", 0)}
              {control(t("studio.develop.sliders.pattern"), "moirePattern", 0)}
            </ToolGroup>
          )}
          {tab === "geometry" && (
            <ToolGroup
              key="crop"
              title="Crop"
              label={t("studio.develop.groups.crop")}
              onReset={() =>
                change({
                  ...Object.fromEntries(
                    ["rotation", "orientationSteps", "flipHorizontal", "flipVertical"].map(
                      (key) => [key, structuredClone(defaults[key])],
                    ),
                  ),
                  // The camera's crop, unturned (src/utils/cameraCrop.ts).
                  ...cropResetValues(selectedImage, values.sourceFrameVersion as number | undefined, 0),
                })
              }
            >
              <StudioCropControls />
            </ToolGroup>
          )}
          {tab === "geometry" && (
            <ToolGroup
              key="geometry"
              title="Geometry"
              label={t("studio.develop.groups.geometry")}
              onReset={() =>
                reset([
                  "transformDistortion",
                  "transformVertical",
                  "transformHorizontal",
                  "transformRotate",
                  "transformScale",
                  "transformAspect",
                  "transformXOffset",
                  "transformYOffset",
                  "guidedPerspective",
                ])
              }
            >
              <StudioUpright />
              {/* Lightroom's manual Transform sliders, on top of Upright. */}
              <div className="studio-subheading">
                {t("studio.develop.geometry.transform")}
              </div>
              {/* Lightroom's manual Distortion, fitted to Camera Raw; the frame
                  is filled without blank corners (Capture One). */}
              {control(
                t("studio.develop.sliders.distortion"),
                "transformDistortion",
              )}
              {control(t("studio.develop.sliders.vertical"), "transformVertical")}
              {control(
                t("studio.develop.sliders.horizontal"),
                "transformHorizontal",
              )}
              {control(
                t("studio.develop.sliders.rotation"),
                "transformRotate",
                -45,
                45,
                0.1,
              )}
              {control(t("studio.develop.sliders.aspect"), "transformAspect")}
              {control(
                t("studio.develop.sliders.scale"),
                "transformScale",
                50,
                150,
              )}
              {control(t("studio.develop.sliders.offsetX"), "transformXOffset")}
              {control(t("studio.develop.sliders.offsetY"), "transformYOffset")}
            </ToolGroup>
          )}
          {tab === "geometry" && <StudioLensCorrection key="lens" />}
          {inTab("all") && !layer && (
            <ToolGroup
              key="vignette"
              title="Vignette"
              label={t("adjustments.effects.vignette")}
              open={false}
              copy={photoOnly([
                ["vignetteAmount", t("adjustments.effects.vignette")],
                ["vignetteMidpoint", t("adjustments.effects.midpoint")],
                ["vignetteRoundness", t("adjustments.effects.roundness")],
                ["vignetteFeather", t("adjustments.effects.feather")],
                ["vignetteStyle", t("studio.develop.effects.style")],
                ["vignetteHighlights", t("studio.develop.sliders.highlights")],
              ])}
              onReset={() => reset(["vignetteAmount", "vignetteMidpoint", "vignetteRoundness", "vignetteFeather", "vignetteStyle", "vignetteHighlights"])}
            >
              {control(t("adjustments.effects.vignette"), "vignetteAmount")}
              {control(t("adjustments.effects.midpoint"), "vignetteMidpoint", 0)}
              {control(t("adjustments.effects.roundness"), "vignetteRoundness")}
              {control(t("adjustments.effects.feather"), "vignetteFeather", 0)}
              <label className="studio-inline">
                <span>{t("studio.develop.effects.style")}</span>
                <select
                  aria-label={t("studio.develop.effects.vignetteStyle")}
                  value={(values.vignetteStyle as string) ?? "highlightPriority"}
                  disabled={!panelsReady}
                  onChange={(e) =>
                    change({
                      vignetteStyle: e.target
                        .value as Adjustments["vignetteStyle"],
                    })
                  }
                >
                  <option value="highlightPriority">
                    {t("studio.develop.effects.styles.highlightPriority")}
                  </option>
                  <option value="colorPriority">
                    {t("studio.develop.effects.styles.colorPriority")}
                  </option>
                  <option value="paintOverlay">
                    {t("studio.develop.effects.styles.paintOverlay")}
                  </option>
                </select>
              </label>
              {values.vignetteStyle !== "paintOverlay" &&
                control(
                  t("studio.develop.sliders.highlights"),
                  "vignetteHighlights",
                  0,
                )}
            </ToolGroup>
          )}
          {inTab("all") && !layer && (
            <ToolGroup
              key="lightEffects"
              title="Light effects"
              label={t("studio.develop.groups.lightEffects")}
              open={false}
              copy={photoOnly([
                ["glowAmount", t("adjustments.effects.glow")],
                ["halationAmount", t("adjustments.effects.halation")],
                ["flareAmount", t("studio.develop.sliders.lightFlare")],
              ])}
              onReset={() => reset(["glowAmount", "halationAmount", "flareAmount"])}
            >
              {control(t("adjustments.effects.glow"), "glowAmount", 0)}
              {control(t("adjustments.effects.halation"), "halationAmount", 0)}
              {control(t("studio.develop.sliders.lightFlare"), "flareAmount", 0)}
            </ToolGroup>
          )}
          {inTab("all") && !layer && (
            <ToolGroup
              key="lut"
              title="LUT"
              label={t("adjustments.effects.lut")}
              open={false}
              copy={photoOnly([
                ["lutPath", "LUT"],
                ["lutIntensity", t("studio.develop.params.lutAmount")],
              ])}
              onReset={() => reset(["lutPath", "lutName", "lutSize", "lutData", "lutIntensity", "lutIsSceneReferred"])}
            >
              <StudioLut disabled={!panelsReady} onInteraction={onInteraction} />
            </ToolGroup>
          )}
          {inTab("all") && !layer && (
            <ToolGroup
              key="relight"
              title="Relight"
              onOpenChange={setRelightPanelOpen}
              label={t("studio.relight.title")}
              open={false}
              toggle={toolSwitch("relight")}
              copy={photoOnly([
                ["relightLights", t("studio.relight.title")],
                ["relightOriginalLight", t("studio.relight.originalLight")],
                ["relightDepthAmount", t("studio.relight.depthAmount")],
                ["relightSkinReflections", t("studio.relight.skinReflections")],
              ])}
              onReset={() =>
                change({
                  relightLights: [],
                  relightDepth: undefined,
                  relightDepthAmount: 60,
                  relightSkinReflections: undefined,
                  relightOriginalLight: 100,
                })
              }
            >
              <StudioRelight
                key={selectedImage?.path}
                disabled={!panelsReady}
                onInteraction={onInteraction}
              />
            </ToolGroup>
          )}
        </ToolColumn>
      </div>
      {/* Snapshots and History stay in reach in every tab, under the tools
          that scroll (Lightroom keeps them in Develop's left column; the UT
          use test found them folded at the end of a long tab). */}
      <div
        className="studio-develop-dock"
        inert={Boolean(selectedImage && !selectedImage.isReady)}
      >
        <ToolColumn scope="develop:dock">
          <StudioSnapshots key="snapshots" />
          <StudioHistory key="history" />
        </ToolColumn>
      </div>
    </div>
  );
}
