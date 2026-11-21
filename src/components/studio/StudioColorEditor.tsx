import { Eye, Pipette, Plus, Target, Trash2 } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useShallow } from 'zustand/react/shallow';
import { useEditorStore } from '../../store/useEditorStore';
import { capturingAdjustments } from '../../hooks/useEditorActions';
import { MAX_COLOR_RANGES, type Adjustments, type ColorRange } from '../../utils/adjustments';
import {
  applyColorPick,
  colorRangesOf,
  invalidatePendingColorPicks,
  isTargetPick,
  skinRangeOf,
  TARGET_PICK,
  targetPickFor
} from '../../utils/colorRanges';
import { oklchCss } from '../../utils/oklab';
import AdjustmentControl from './AdjustmentControl';
import StudioColorRangeWheel from './StudioColorRangeWheel';
import StudioColorTarget from './StudioColorTarget';
import { ToolGroup } from './StudioDevelop';
import type { ToolParams } from './StudioToolCopy';

interface Props {
  values: Partial<Adjustments>;
  /** The selected mask layer, when the tool edits a layer. */
  layerId?: string;
  disabled: boolean;
  change(patch: Partial<Adjustments>): void;
  onInteraction(active: boolean): void;
  toggle?: { enabled: boolean; onToggle(): void };
  copy?: ToolParams;
}

/** The range's sliders (their accessible names name the group). */
type SliderKey =
  | 'hueShift'
  | 'saturation'
  | 'lightnessShift'
  | 'hueRange'
  | 'chromaRange'
  | 'lightnessRange'
  | 'smoothness'
  | 'hueUniformity'
  | 'saturationUniformity'
  | 'lightnessUniformity';

/** Swatch of a range after its own hue, saturation and lightness changes. */

export function rangeSwatch(range: ColorRange, shifted = false) {
  if (!shifted) return oklchCss(range.lightness, range.chroma, range.hue);
  return oklchCss(
    Math.min(1, Math.max(0, range.lightness * (1 + range.lightnessShift * 0.004))),
    Math.max(0, range.chroma * (1 + range.saturation / 100)),
    (range.hue + range.hueShift * 0.5 + 360) % 360
  );
}

/** Lightness a range picked on the wheel starts with (the wheel has none). */
const WHEEL_PICK_LIGHTNESS = 0.65;

type Mode = 'ranges' | 'skin';

/**
 * Capture One's Advanced Color Editor / Lightroom's Point Color: pick a
 * colour in the photo (or on the range wheel), then shift the hue,
 * saturation and lightness of that colour range, shape the range on the
 * wheel or with the sliders, and even it out (uniformity). Up to eight
 * ranges; Show range greys out everything else. The Skin tone mode holds
 * one range of its own for skin: pick skin, then even it out (uniformity)
 * and move it (amount).
 */
export default function StudioColorEditor({ values, layerId, disabled, change, onInteraction, toggle, copy }: Props) {
  const { t } = useTranslation();
  const { photo, pickerTarget, activeId, showRange, setEditor } = useEditorStore(
    useShallow((s) => ({
      photo: s.selectedImage?.path,
      pickerTarget: s.colorPickerTarget,
      activeId: s.activeColorRangeId,
      showRange: s.colorRangeShow,
      setEditor: s.setEditor
    }))
  );
  // A pick armed here (a new range, picking one again, its target) ends when
  // this tool turns to another layer or goes away: the click would otherwise
  // edit ranges no longer shown. Sampling a target edits nothing and stays.
  useEffect(
    () => () => {
      const state = useEditorStore.getState();
      const armed = state.colorPickerTarget;
      if (armed && armed !== TARGET_PICK && state.colorPickerLayer === (layerId ?? null))
        state.setEditor({ colorPickerTarget: null, colorPickerLayer: null });
    },
    [layerId]
  );
  const ranges = colorRangesOf(values);
  const skin = skinRangeOf(ranges);
  const [mode, setMode] = useState<Mode>(skin && skin.id === activeId ? 'skin' : 'ranges');
  const listed = ranges.filter((r) => r.kind !== 'skin');
  const active = mode === 'skin' ? skin : (listed.find((r) => r.id === activeId) ?? listed[0]);
  const setRange = (patch: Partial<ColorRange>) =>
    active &&
    change({
      colorRanges: ranges.map((r) => (r.id === active.id ? { ...r, ...patch } : r))
    });
  const limit = MAX_COLOR_RANGES;
  const rangePickArmed =
    !!pickerTarget && pickerTarget !== 'new' && pickerTarget !== 'skin' && pickerTarget !== TARGET_PICK;
  const hue = t('adjustments.color.hue');
  const saturation = t('adjustments.color.saturation');
  const lightness = t('studio.color.editor.lightness');
  const pick = (target: string) =>
    setEditor({
      colorPickerTarget: pickerTarget === target ? null : target,
      colorPickerLayer: layerId ?? null,
      isWbPickerActive: false
    });
  /** A press on the wheel while picking (or with no range yet). */
  const pickOnWheel = (hue: number, chroma: number) => {
    const armed = isTargetPick(pickerTarget) ? null : pickerTarget;
    const target = mode === 'skin' ? 'skin' : (armed ?? 'new');
    const picked = applyColorPick(
      ranges,
      target,
      limit,
      hue,
      chroma,
      (target === 'skin' ? skin : ranges.find((r) => r.id === target))?.lightness ?? WHEEL_PICK_LIGHTNESS
    );
    if (!picked.id) return;
    invalidatePendingColorPicks();
    setEditor({
      colorPickerTarget: null,
      colorPickerLayer: null,
      activeColorRangeId: picked.id
    });
    change({ colorRanges: picked.ranges });
  };
  const switchMode = (next: Mode) => {
    if (next === mode) return;
    setMode(next);
    setEditor({
      activeColorRangeId: next === 'skin' ? (skin?.id ?? null) : null,
      colorPickerTarget: null,
      colorRangeShow: null
    });
  };
  // A pick armed for a range (or a new one) hands the wheel's next press to
  // the pick instead of moving the active range.
  // Only a pick that adds a range needs room below the limit.
  const wheelAdds = mode === 'skin' ? !skin : pickerTarget === 'new';
  const wheelPicking =
    mode === 'skin'
      ? !skin || pickerTarget === 'skin' || pickerTarget === skin.id
      : pickerTarget === 'new' || (!!pickerTarget && listed.some((r) => r.id === pickerTarget));
  const slider = (label: string, key: SliderKey, min: number, max: number, fallback: number) =>
    active && (
      <AdjustmentControl
        key={key}
        label={label}
        ariaLabel={t(`studio.color.editor.aria.${key}`)}
        value={(active[key] as number) ?? fallback}
        min={min}
        max={max}
        defaultValue={fallback}
        disabled={disabled}
        onInteraction={onInteraction}
        onChange={(v) => setRange({ [key]: v })}
      />
    );
  const actions = active && (
    <div className="studio-inline studio-color-range-actions">
      <button
        type="button"
        aria-pressed={pickerTarget === active.id}
        title={t('studio.color.editor.pickAgainTitle')}
        aria-label={t('studio.color.editor.pickAgainAria')}
        disabled={disabled}
        onClick={() => pick(active.id)}
      >
        <Pipette size={14} />
      </button>
      {!layerId && (
        <button
          type="button"
          aria-pressed={showRange === active.id}
          title={t('studio.color.editor.showTitle')}
          aria-label={t('studio.color.editor.showAria')}
          disabled={disabled}
          onClick={() =>
            setEditor({
              colorRangeShow: showRange === active.id ? null : active.id
            })
          }
        >
          <Eye size={14} />
        </button>
      )}
      <button
        type="button"
        title={t('studio.color.editor.removeTitle')}
        aria-label={t('studio.color.editor.removeAria')}
        disabled={disabled}
        onClick={() => {
          setEditor({
            activeColorRangeId: null,
            ...(showRange === active.id ? { colorRangeShow: null } : {})
          });
          change({
            colorRanges: ranges.filter((r) => r.id !== active.id)
          });
        }}
      >
        <Trash2 size={14} />
      </button>
    </div>
  );
  const wheel = (
    <StudioColorRangeWheel
      // A new editing target (photo, layer, range) ends a drag on the old
      // one: its pending geometry is dropped, never written elsewhere.
      key={`${photo}:${layerId ?? 'photo'}:${mode}:${wheelPicking ? 'pick' : (active?.id ?? 'none')}`}
      ranges={mode === 'skin' ? (skin ? [skin] : []) : listed}
      active={wheelPicking ? undefined : active}
      disabled={disabled || (wheelAdds && ranges.length >= limit)}
      onChange={setRange}
      onPick={pickOnWheel}
      onInteraction={onInteraction}
    />
  );
  const shape = active && (
    <>
      <div className="studio-subheading">{t('studio.color.editor.range')}</div>
      {slider(hue, 'hueRange', 5, 90, 25)}
      {slider(saturation, 'chromaRange', 0, 100, 70)}
      {slider(lightness, 'lightnessRange', 0, 100, 100)}
      {slider(t('studio.color.editor.smoothness'), 'smoothness', 0, 100, 50)}
    </>
  );
  const uniformity = active && (
    <>
      <div className="studio-subheading">{t('studio.color.editor.uniformity')}</div>
      {slider(hue, 'hueUniformity', 0, 100, 0)}
      {slider(saturation, 'saturationUniformity', 0, 100, 0)}
      {slider(lightness, 'lightnessUniformity', 0, 100, 0)}
    </>
  );
  const shifts = active && (
    <>
      {slider(hue, 'hueShift', -100, 100, 0)}
      {slider(saturation, 'saturation', -100, 100, 0)}
      {slider(lightness, 'lightnessShift', -100, 100, 0)}
      <StudioColorTarget
        ranges={ranges}
        active={active}
        layerId={layerId}
        disabled={disabled}
        apply={(next) => change({ colorRanges: next })}
      />
    </>
  );
  // Samples a target colour only (to use in this or another photo).
  const sampleTarget = (
    <button
      type="button"
      className="studio-color-range-add"
      aria-pressed={pickerTarget === TARGET_PICK}
      aria-label={t('studio.color.editor.target.sampleAria')}
      title={t('studio.color.editor.target.sampleTitle')}
      disabled={disabled}
      onClick={() => pick(TARGET_PICK)}
    >
      <Target size={13} />
    </button>
  );
  return (
    <ToolGroup
      title="Color editor"
      label={t('studio.color.editor.title')}
      toggle={toggle}
      copy={copy}
      open={ranges.length > 0}
      onReset={() => {
        // Its per-tool before (a held reset) changes no view state.
        if (!capturingAdjustments())
          setEditor({
            activeColorRangeId: null,
            colorPickerTarget: null,
            colorRangeShow: null
          });
        change({ colorRanges: [] });
      }}
    >
      <div
        className="studio-segments studio-color-editor-modes"
        role="tablist"
        aria-label={t('studio.color.editor.modesAria')}
      >
        {(['ranges', 'skin'] as const).map((m) => (
          <button key={m} type="button" role="tab" aria-selected={mode === m} onClick={() => switchMode(m)}>
            {t(`studio.color.editor.mode.${m}`)}
          </button>
        ))}
      </div>
      {mode === 'ranges' && (
        <div className="studio-color-ranges">
          {listed.map((range) => (
            <button
              key={range.id}
              type="button"
              className="studio-color-range"
              aria-pressed={active?.id === range.id}
              aria-label={t('studio.color.editor.rangeAria', {
                hue: Math.round(range.hue)
              })}
              title={t('studio.color.editor.selectRange')}
              style={{
                background: `linear-gradient(90deg, ${rangeSwatch(range)} 50%, ${rangeSwatch(range, true)} 50%)`
              }}
              onClick={() =>
                setEditor({
                  activeColorRangeId: range.id,
                  // A pick armed for another range (picking it again or its
                  // target) would edit a range no longer shown: end it.
                  ...(rangePickArmed && pickerTarget !== targetPickFor(range.id) && pickerTarget !== range.id
                    ? { colorPickerTarget: null, colorPickerLayer: null }
                    : {})
                })
              }
            />
          ))}
          <button
            type="button"
            className="studio-color-range-add"
            aria-pressed={pickerTarget === 'new'}
            aria-label={t('studio.color.editor.pickAria')}
            title={t('studio.color.editor.pickTitle')}
            disabled={disabled || ranges.length >= limit}
            onClick={() => pick('new')}
          >
            <Plus size={13} />
          </button>
          {sampleTarget}
        </div>
      )}
      {mode === 'skin' && !skin && (
        <div className="studio-inline studio-color-range-actions">
          <button
            type="button"
            aria-pressed={pickerTarget === 'skin'}
            title={t('studio.color.editor.skin.pickTitle')}
            aria-label={t('studio.color.editor.skin.pickAria')}
            disabled={disabled || ranges.length >= limit}
            onClick={() => pick('skin')}
          >
            <Pipette size={14} />
          </button>
          {sampleTarget}
        </div>
      )}
      {!active && (
        <p className="studio-hint">
          {mode === 'skin'
            ? pickerTarget === 'skin'
              ? t('studio.color.editor.skin.hintPicking')
              : t('studio.color.editor.skin.hintEmpty')
            : pickerTarget === 'new'
              ? t('studio.color.editor.hintPicking')
              : t('studio.color.editor.hintEmpty')}
        </p>
      )}
      {wheel}
      {active && mode === 'ranges' && (
        <>
          {actions}
          {shifts}
          {shape}
          {uniformity}
        </>
      )}
      {active && mode === 'skin' && (
        <>
          {actions}
          {uniformity}
          <div className="studio-subheading">{t('studio.color.editor.skin.amount')}</div>
          {shifts}
          {shape}
        </>
      )}
    </ToolGroup>
  );
}
