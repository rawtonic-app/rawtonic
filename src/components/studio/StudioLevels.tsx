import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { Pipette, Sparkles } from "lucide-react";
import { useEditorStore } from "../../store/useEditorStore";
import { INITIAL_LEVELS, type Adjustments, type Level, type Levels } from "../../utils/adjustments";
import {
  LEVEL_CHANNELS,
  LEVEL_DEFAULTS,
  autoLevels,
  handlePosition,
  levelOf,
  levelValue,
  levelsInputHistogram,
  nudgedLevel,
  withHandleAt,
  withLevelValue,
  type LevelChannel,
  type LevelValue,
} from "../../utils/levels";
import type { LevelsPicker } from "../../utils/targetedAdjustment";
import { selectOnFocus } from "../../utils/selectOnFocus";
import { ToolGroup } from "./StudioDevelop";
import { histogramBins } from "./StudioHistogram";
import type { ToolParams } from "./StudioToolCopy";

interface LevelsProps {
  values: Partial<Adjustments>;
  disabled: boolean;
  change(patch: Partial<Adjustments>): void;
  onInteraction(active: boolean): void;
  toggle?: { enabled: boolean; onToggle(): void };
  copy?: ToolParams;
}

const TAB_LABELS: Record<LevelChannel, string> = {
  rgb: "RGB",
  red: "R",
  green: "G",
  blue: "B",
};
const COLOURS = ["red", "green", "blue"] as const;
const INPUT_HANDLES: LevelValue[] = ["inBlack", "midtone", "inWhite"];
const OUTPUT_HANDLES: LevelValue[] = ["outBlack", "outWhite"];
const RANGES: Record<LevelValue, [number, number]> = {
  inBlack: [0, 254],
  midtone: [-100, 100],
  inWhite: [1, 255],
  outBlack: [0, 255],
  outWhite: [0, 255],
};

/** A numeric field of one value: Enter or leaving commits, Escape cancels,
 * arrows nudge (Shift ×10), double-click resets. */
function LevelField({
  name,
  value,
  disabled,
  onCommit,
  onNudge,
  onReset,
}: {
  name: string;
  value: number;
  disabled: boolean;
  onCommit(value: number): void;
  onNudge(event: KeyboardEvent<HTMLInputElement>): void;
  onReset(): void;
}) {
  const { t } = useTranslation();
  const [text, setText] = useState(String(value));
  const editing = useRef(false);
  const canceled = useRef(false);
  useEffect(() => {
    if (!editing.current) setText(String(value));
  }, [value]);
  return (
    <input
      className="studio-levels-field"
      aria-label={t("studio.tone.levels.fieldAria", { name })}
      title={name}
      inputMode="numeric"
      type="text"
      value={text}
      disabled={disabled}
      onMouseDown={selectOnFocus.onMouseDown}
      onMouseUp={selectOnFocus.onMouseUp}
      onFocus={(e) => {
        editing.current = true;
        selectOnFocus.onFocus(e);
      }}
      onChange={(e) => {
        canceled.current = false;
        editing.current = true;
        setText(e.target.value);
      }}
      onBlur={() => {
        editing.current = false;
        const typed = Number(text);
        if (!canceled.current && text.trim() && Number.isFinite(typed) && typed !== value)
          onCommit(typed);
        canceled.current = false;
        setText(String(value));
      }}
      onDoubleClick={() => {
        // The reset value shows at once and leaving does not commit the
        // text from before it.
        canceled.current = true;
        editing.current = false;
        onReset();
      }}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") e.currentTarget.blur();
        else if (e.key === "Escape") {
          canceled.current = true;
          e.currentTarget.blur();
        } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
          e.preventDefault();
          onNudge(e);
          editing.current = false;
        }
      }}
    />
  );
}

/**
 * Capture One's Levels: the histogram of the channel (R, G and B overlaid
 * under RGB) with input handles for the black point, midtone (gamma) and
 * white point, output handles below, a field for each of the five values
 * and black and white point pickers, for the RGB composite and each
 * channel; applied to display values before the curve. Auto sets the input
 * points from the histogram. One undo step per drag.
 */
export function StudioLevels({
  values,
  disabled,
  change,
  onInteraction,
  toggle,
  copy,
}: LevelsProps) {
  const { t } = useTranslation();
  const channel = useEditorStore((s) => s.levelsChannel);
  const tatTarget = useEditorStore((s) => s.tatTarget);
  const setEditor = useEditorStore((s) => s.setEditor);
  const histogram = useEditorStore((s) => s.histogram);
  const levels: Levels = { ...INITIAL_LEVELS, ...values.levels };
  const committed: Level = levelOf(levels, channel);
  // The level being edited shows at once: a drag's moves reach the recipe
  // in batches (sliderDraft), and the handles and fields follow the pointer.
  const [draft, setDraft] = useState<{ channel: LevelChannel; level: Level } | null>(null);
  const current = draft?.channel === channel ? draft.level : committed;
  const latest = useRef(current);
  latest.current = current;
  const set = (level: Level) => {
    latest.current = level;
    setDraft({ channel, level });
    change({ levels: { ...levels, [channel]: level } });
  };
  const setValue = (key: LevelValue, value: number) =>
    set(withLevelValue(latest.current, key, value));
  const reset = (key: LevelValue) => {
    if (!disabled) setValue(key, LEVEL_DEFAULTS[key]);
  };
  const nudge = (key: LevelValue, e: KeyboardEvent) => {
    const next = nudgedLevel(latest.current, key, e);
    if (!next) return false;
    e.preventDefault();
    e.stopPropagation();
    if (!disabled) set(next);
    return true;
  };
  const names: Record<LevelValue, string> = {
    inBlack: t("studio.tone.levels.inBlack"),
    midtone: t("studio.tone.levels.midtone"),
    inWhite: t("studio.tone.levels.inWhite"),
    outBlack: t("studio.tone.levels.outBlack"),
    outWhite: t("studio.tone.levels.outWhite"),
  };
  const drag = useRef<{ key: LevelValue; strip: DOMRect } | null>(null);
  // Outside a drag the recipe's values win again once they arrive.
  const committedKey = JSON.stringify(committed);
  useEffect(() => {
    if (!drag.current) setDraft(null);
  }, [committedKey, channel]);
  const endDrag = () => {
    if (!drag.current) return;
    drag.current = null;
    onInteraction(false);
  };
  // A drag still running when the tool unmounts ends its history step.
  const endRef = useRef(endDrag);
  endRef.current = endDrag;
  useEffect(() => () => endRef.current(), []);
  const handle = (key: LevelValue) => (
    <div
      key={key}
      role="slider"
      tabIndex={disabled ? -1 : 0}
      className={`studio-levels-handle is-${key}`}
      style={{ left: `${Math.max(0, Math.min(1, handlePosition(current, key))) * 100}%` }}
      aria-label={names[key]}
      aria-valuemin={RANGES[key][0]}
      aria-valuemax={RANGES[key][1]}
      aria-valuenow={levelValue(current, key)}
      aria-disabled={disabled || undefined}
      title={names[key]}
      onPointerDown={(e: PointerEvent<HTMLDivElement>) => {
        if (disabled || e.button !== 0) return;
        const strip = e.currentTarget.parentElement?.getBoundingClientRect();
        if (!strip?.width) return;
        e.preventDefault();
        e.currentTarget.focus();
        try {
          e.currentTarget.setPointerCapture(e.pointerId);
        } catch {
          // A pointer the browser does not track (synthetic input).
        }
        drag.current = { key, strip };
        onInteraction(true);
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (!d || d.key !== key) return;
        const next = withHandleAt(latest.current, key, (e.clientX - d.strip.left) / d.strip.width);
        if (levelValue(next, key) !== levelValue(latest.current, key)) set(next);
      }}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onLostPointerCapture={endDrag}
      onDoubleClick={() => reset(key)}
      onKeyDown={(e) => nudge(key, e)}
    />
  );
  const field = (key: LevelValue) => (
    <LevelField
      key={key}
      name={names[key]}
      value={levelValue(current, key)}
      disabled={disabled}
      onCommit={(value) => setValue(key, value)}
      onNudge={(e) => nudge(key, e)}
      onReset={() => reset(key)}
    />
  );
  const shown = channel === "rgb" ? COLOURS : ([channel] as const);
  const rendered = shown.map((c) => histogramBins(histogram?.[c]));
  // Drawn as what Levels receive, so the handles sit on the tones they
  // move (through the recipe's levels, which the histogram was rendered with).
  const bins = shown.map((c, i) =>
    levelsInputHistogram(rendered[i], levels, channel, c),
  );
  const max = Math.max(1, ...bins.flat());
  const auto = () => {
    if (!histogram) return;
    const bins = rendered;
    const next = autoLevels(bins, current);
    if (next) set({ ...current, ...next });
  };
  const picker = (target: LevelsPicker, label: string, hint: string) => (
    <button
      type="button"
      className={`studio-levels-picker is-${target}`}
      aria-label={label}
      aria-pressed={tatTarget === target}
      title={hint}
      disabled={disabled}
      onClick={(e) => {
        e.preventDefault();
        setEditor({
          tatTarget: tatTarget === target ? null : target,
          isWbPickerActive: false,
          colorPickerTarget: null,
        });
      }}
    >
      <Pipette size={12} />
    </button>
  );
  return (
    <ToolGroup
      title="Levels"
      label={t("studio.tone.levels.title")}
      toggle={toggle}
      copy={copy}
      open={false}
      actions={
        <button
          type="button"
          className="studio-tool-auto"
          aria-label={t("studio.tone.levels.autoAria")}
          disabled={disabled || !histogram}
          title={t("studio.tone.levels.autoTitle")}
          onClick={(e) => {
            e.preventDefault();
            auto();
          }}
        >
          <Sparkles size={12} />
          {t("studio.tone.levels.auto")}
        </button>
      }
      onReset={() => change({ levels: structuredClone(INITIAL_LEVELS) })}
    >
      <div className="studio-levels-top">
        {picker("levelsBlack", t("studio.tone.levels.pickBlack"), t("studio.tone.levels.pickBlackTitle"))}
        <div className="studio-segments" aria-label={t("studio.tone.levels.channelAria")}>
          {LEVEL_CHANNELS.map((key) => (
            <button
              key={key}
              type="button"
              aria-pressed={channel === key}
              onClick={() => setEditor({ levelsChannel: key })}
            >
              {TAB_LABELS[key]}
            </button>
          ))}
        </div>
        {picker("levelsWhite", t("studio.tone.levels.pickWhite"), t("studio.tone.levels.pickWhiteTitle"))}
      </div>
      <div className={`studio-levels ${disabled ? "is-disabled" : ""}`}>
        <svg
          className="studio-levels-histogram"
          viewBox="0 0 256 64"
          preserveAspectRatio="none"
          role="img"
          aria-label={t("studio.tone.levels.histogramAria")}
        >
          {bins.map((values, i) =>
            values.length ? (
              <path
                key={shown[i]}
                className={`histogram-${shown[i]}`}
                d={`M0 64 ${values.map((v, k) => `L${(k * 256) / Math.max(1, values.length - 1)} ${64 - Math.sqrt(v / max) * 62}`).join(" ")} L256 64 Z`}
              />
            ) : null,
          )}
          {INPUT_HANDLES.map((key) => {
            const x = handlePosition(current, key) * 256;
            return (
              <line
                key={key}
                className={`studio-levels-guide is-${key}`}
                x1={x}
                x2={x}
                y1="0"
                y2="64"
              />
            );
          })}
        </svg>
        <div className="studio-levels-strip">{INPUT_HANDLES.map(handle)}</div>
        <div className="studio-levels-fields">{INPUT_HANDLES.map(field)}</div>
        <div className="studio-subheading">{t("studio.tone.levels.output")}</div>
        <div className={`studio-levels-ramp is-${channel}`} />
        <div className="studio-levels-strip">{OUTPUT_HANDLES.map(handle)}</div>
        <div className="studio-levels-fields">{OUTPUT_HANDLES.map(field)}</div>
      </div>
    </ToolGroup>
  );
}
