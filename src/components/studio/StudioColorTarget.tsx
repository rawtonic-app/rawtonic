import { Pipette } from 'lucide-react';
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { useShallow } from 'zustand/react/shallow';
import { useEditorStore } from '../../store/useEditorStore';
import type { ColorRange } from '../../utils/adjustments';
import {
  applyColorTarget,
  cancelColorTargetPreview,
  measureColorTarget,
  previewColorTarget,
  rangeInputFor,
  rangePickKey,
  rangePickOf,
  targetPickFor
} from '../../utils/colorRanges';
import {
  centreInput,
  deltaE00,
  hexOf,
  insideSrgb,
  labToLch,
  labToLinear,
  lchToLab,
  linearToLab,
  linearToSrgb8,
  oklabToLinear,
  parseHex,
  rangeCentre,
  renderedLab,
  SHIFT_LIMIT,
  targetStatus,
  srgb8ToLinear,
  TARGET_REACHED,
  type Vec3
} from '../../utils/colorTarget';

const clip01 = (lin: Vec3) => lin.map((v) => Math.min(1, Math.max(0, v))) as Vec3;
const cssOf = (lab: Vec3) => {
  const [r, g, b] = linearToSrgb8(clip01(labToLinear(lab))).map((v) => Math.round(v));
  return `rgb(${r} ${g} ${b})`;
};
const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** Typed values redraw the photo this long after the last key (Capture One
 * redraws while the digits are typed). */
export const TYPED_PREVIEW_MS = 50;

/** A field that redraws the photo with each valid value typed, commits on
 * Return or when left (one undo step) and restores on Escape. Return and
 * Escape leave the field, so the app's shortcuts (Undo) work again. */
function Field({
  value,
  label,
  className,
  disabled,
  commit,
  preview
}: {
  value: string;
  label: string;
  className?: string;
  disabled: boolean;
  /** False when the text is not a valid value (the field then reverts). */
  commit(text: string): boolean;
  /** Redraws with the typed text; false (nothing drawn) when it is not a valid value. */
  preview?(text: string): boolean;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  // The text being edited, read when the field is left: a blur that follows
  // a key in the same event sees it before React re-renders.
  const editing = useRef<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Whether the photo shows a typed value not committed yet.
  const previewed = useRef(false);
  const stopTimer = () => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = null;
  };
  useEffect(() => stopTimer, []);
  const edit = (text: string | null) => {
    editing.current = text;
    setDraft(text);
    stopTimer();
    if (text !== null && preview)
      timer.current = setTimeout(() => {
        timer.current = null;
        if (preview(text)) previewed.current = true;
      }, TYPED_PREVIEW_MS);
  };
  const finish = () => {
    const text = editing.current;
    edit(null);
    const shown = previewed.current;
    previewed.current = false;
    if (text !== null && (shown || text.trim() !== value)) {
      if (!commit(text) && shown) cancelColorTargetPreview();
    } else if (shown) cancelColorTargetPreview();
  };
  return (
    <input
      type="text"
      className={`studio-color-target-field ${className ?? ''}`}
      aria-label={label}
      title={label}
      spellCheck={false}
      disabled={disabled}
      value={draft ?? value}
      onChange={(e) => edit(e.target.value)}
      onBlur={finish}
      onKeyDown={(e: KeyboardEvent<HTMLInputElement>) => {
        // The field's own keys: the app's shortcuts (Escape leaves Develop)
        // must not act on them once the field has let go of the focus.
        if (e.key === 'Enter' || e.key === 'Escape') e.preventDefault();
        if (e.key === 'Enter') {
          finish();
          e.currentTarget.blur();
        } else if (e.key === 'Escape') {
          edit(null);
          if (previewed.current) cancelColorTargetPreview();
          previewed.current = false;
          e.currentTarget.blur();
        }
      }}
    />
  );
}

interface Props {
  /** The ranges of the photo or layer the tool edits, with the active one. */
  ranges: ColorRange[];
  active: ColorRange;
  layerId?: string;
  disabled: boolean;
  /** Applies the ranges (one edit, one undo step). */
  apply(ranges: ColorRange[]): void;
}

/**
 * Target colour for the active range: typed (hex, sRGB, L*C*h° D50), picked
 * in the photo, or sampled in another photo. The range's own hue,
 * saturation and lightness are solved so that its sampled colour lands on
 * the target; the readout says how close it got and what stopped it.
 */
export default function StudioColorTarget({ ranges, active, layerId, disabled, apply }: Props) {
  const { t } = useTranslation();
  const { target, photo, pickerTarget, through, measure, setEditor } = useEditorStore(
    useShallow((s) => ({
      target: s.colorTarget,
      measure: s.colorTargetMeasure,
      through: s.colorPicksThroughRanges,
      // Re-renders when the renderer's sample at a pick arrives.
      inputs: s.colorRangeInputs,
      photo: s.selectedImage?.path ?? null,
      pickerTarget: s.colorPickerTarget,
      setEditor: s.setEditor
    }))
  );
  const adjustments = useEditorStore((s) => s.adjustments);
  const layer = layerId ?? null;
  const latestActive = useRef(active);
  latestActive.current = active;
  // The panel's ranges and edit, read when a target's solve comes back.
  const latest = useRef({ ranges, apply });
  latest.current = { ranges, apply };
  const io = useMemo(
    () => ({ ranges: () => latest.current.ranges, apply: (next: ColorRange[]) => latest.current.apply(next) }),
    []
  );
  const own =
    target?.appliedTo &&
    target.appliedTo.photo === photo &&
    target.appliedTo.layer === layer &&
    target.appliedTo.rangeId === active.id
      ? target
      : null;
  const centre = rangeCentre(active);
  // The renderer's sample at the range's pick (the colour entering the
  // colour editor and the layer's mask influence), else the range's colour
  // at full strength.
  const pick = photo ? rangePickOf(photo, layer, active) : null;
  const pickInput = pick?.input ?? null;
  const input = useMemo(
    () => pickInput ?? centreInput(active),
    // The range's sampled colour, not its shifts, makes the stand-in.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [pickInput, active.hue, active.chroma, active.lightness]
  );
  // The measured result at the pick, for the recipe as it is: the readout
  // states it, never the model's prediction.
  const followed = !!own && !!pick;
  const recipe = useMemo(() => (followed ? JSON.stringify(adjustments) : null), [followed, adjustments]);
  const measured =
    followed && photo && measure?.key === rangePickKey(photo, layer, active.id) && measure.recipe === recipe
      ? measure
      : null;
  const measuredLab = measured?.lab ?? null;
  // The renderer answered for this recipe but could not measure it (AI
  // Denoise): the readout is the model's prediction, and says so.
  const unmeasurable = !!measured && !measuredLab;
  const measuring = followed && !measured;
  // Edits before the colour editor or of the layer's mask change what it
  // receives at the pick, and any edit what it leaves there: sample both
  // again once they settle, so the readout follows them.
  useEffect(() => {
    if (!photo || !measuring) return;
    const timer = setTimeout(() => {
      const range = latestActive.current;
      void rangeInputFor(photo, layer, range).then(() => measureColorTarget(photo, layer, range));
    }, 250);
    return () => clearTimeout(timer);
  }, [adjustments, photo, layer, active.id, measuring]);
  // The colour at the pick as the renderer leaves it (the sliders as they are).
  const resultLab = renderedLab(ranges, input);
  const shown = own ? own.lab : resultLab;
  const shownLinear = labToLinear(shown);
  const rgb = linearToSrgb8(shownLinear);
  const [L, C, h] = labToLch(shown);

  // One edit: the model's solve at once, then corrected on the renderer's
  // measured result at the pick (utils/colorRanges `applyColorTarget`).
  const setTarget = (lab: Vec3 | null, sampledIn: string | null = null) => {
    if (!photo || !lab || !lab.every(Number.isFinite)) return false;
    void applyColorTarget(photo, layer, active.id, lab, sampledIn, io);
    return true;
  };
  // A valid value typed, before Return: redrawn with the model's solve.
  const previewTarget = (lab: Vec3 | null) => {
    if (!photo || !lab || !lab.every(Number.isFinite)) return false;
    previewColorTarget(photo, layer, active.id, lab, io);
    return true;
  };
  const number = (text: string) => {
    const v = Number(text.replace(',', '.'));
    return text.trim() !== '' && Number.isFinite(v) ? v : null;
  };
  const rgbLab = (i: 0 | 1 | 2, text: string) => {
    const v = number(text);
    if (v === null || v < 0 || v > 255) return null;
    const next = [...rgb] as Vec3;
    next[i] = v;
    return linearToLab(srgb8ToLinear(next));
  };
  const rgbField = (i: 0 | 1 | 2, label: string) => (
    <Field
      key={label}
      label={label}
      disabled={disabled}
      value={String(Math.round(rgb[i]))}
      commit={(text) => setTarget(rgbLab(i, text))}
      preview={(text) => previewTarget(rgbLab(i, text))}
    />
  );
  const lchLab = (i: 0 | 1 | 2, text: string, max: number) => {
    const v = number(text);
    if (v === null || v < 0 || v > max) return null;
    const next: Vec3 = [L, C, h];
    next[i] = i === 2 ? v % 360 : v;
    return lchToLab(next);
  };
  const lchField = (i: 0 | 1 | 2, label: string, max: number, digits: number) => (
    <Field
      key={label}
      label={label}
      disabled={disabled}
      value={[L, C, h][i].toFixed(digits)}
      commit={(text) => setTarget(lchLab(i, text, max))}
      preview={(text) => previewTarget(lchLab(i, text, max))}
    />
  );
  const hexLab = (text: string) => {
    const parsed = parseHex(text);
    return parsed && linearToLab(srgb8ToLinear(parsed));
  };

  // The readout's status of the sliders as they are (no search: it follows
  // every slider move).
  const solve = own ? targetStatus(ranges, active.id, own.lab, input) : null;
  // With a pick in the photo the readout states the renderer's measured
  // result there (TGT3: a prediction of 0.1 exported 2.36 off); only a range
  // without a pick is predicted, and says so.
  const measuredDeltaE = own && measuredLab ? deltaE00(own.lab, measuredLab) : null;
  const deltaE = measuredDeltaE ?? solve?.deltaE ?? 0;
  const keys = ['hue', 'saturation', 'lightness'] as const;
  const shifts = { hue: active.hueShift, saturation: active.saturation, lightness: active.lightnessShift };
  const limits = solve
    ? keys
        .filter((k) =>
          measuredDeltaE === null
            ? solve.limited[k]
            : measuredDeltaE > TARGET_REACHED && Math.abs(shifts[k] ?? 0) >= SHIFT_LIMIT
        )
        .map((k) => t(`studio.color.editor.target.limit.${k}`))
    : [];
  const grey = !!solve && !solve.movable;
  const value = deltaE.toFixed(1);
  // The range was last picked through a strong change of the ranges: its
  // colour, and so this prediction, is uncertain.
  const pickedThrough = !!photo && through.includes(rangePickKey(photo, layer, active.id));
  // A colour picked at the edge of sRGB was clipped by the display: the
  // photo's own colour lies beyond it and may move differently
  // (docs/benchmark-results/tgt1-target-colour-2026-11-21).
  // With the renderer's sample the colour is the photo's own.
  const sourceClipped = !pickInput && linearToSrgb8(oklabToLinear(centre)).some((v) => v <= 1 || v >= 254);
  // A layer's prediction is at full strength without the mask influence at the pick.
  const fullStrength = !!layerId && !pickInput && !followed;
  const beyondSrgb = !insideSrgb(shownLinear);
  const readout = () => {
    if (solve?.uncovered) return t('studio.color.editor.target.uncovered');
    if (grey) return t('studio.color.editor.target.grey');
    if (measuring) return t('studio.color.editor.target.measuring');
    if (measuredDeltaE !== null) {
      if (deltaE <= TARGET_REACHED) return t('studio.color.editor.target.measuredReached', { value });
      if (beyondSrgb) return t('studio.color.editor.target.measuredOutsideSrgb', { value });
      if (limits.length)
        return t('studio.color.editor.target.measuredOutOfReach', { value, limits: limits.join(', ') });
      return t('studio.color.editor.target.measuredDistance', { value });
    }
    if (unmeasurable) return t('studio.color.editor.target.notMeasured', { value });
    if (pickedThrough) return t('studio.color.editor.target.pickedThrough', { value });
    if (deltaE <= TARGET_REACHED && fullStrength) return t('studio.color.editor.target.layerPredicted', { value });
    if (limits.length && deltaE > TARGET_REACHED)
      return t('studio.color.editor.target.predictedOutOfReach', { value, limits: limits.join(', ') });
    return t('studio.color.editor.target.predicted', { value });
  };
  // A sampled target not on this range yet (from another photo, or sampled
  // here without a range).
  const sampled = target?.sampledIn && !own ? target : null;
  const pickArmed = pickerTarget === targetPickFor(active.id);

  return (
    <div className="studio-color-target" role="group" aria-label={t('studio.color.editor.target.heading')}>
      <div className="studio-subheading studio-color-target-head">
        <span>{t('studio.color.editor.target.heading')}</span>
        <span
          className="studio-color-target-swatch"
          role="img"
          aria-label={t('studio.color.editor.target.swatchAria', {
            result: hexOf(linearToSrgb8(labToLinear(measuredLab ?? resultLab))),
            target: hexOf(rgb)
          })}
          style={{
            background: `linear-gradient(90deg, ${cssOf(measuredLab ?? resultLab)} 50%, ${cssOf(shown)} 50%)`
          }}
        />
        <button
          type="button"
          aria-pressed={pickArmed}
          title={t('studio.color.editor.target.pickTitle')}
          aria-label={t('studio.color.editor.target.pickAria')}
          disabled={disabled}
          onClick={() =>
            setEditor({
              colorPickerTarget: pickArmed ? null : targetPickFor(active.id),
              colorPickerLayer: layer,
              isWbPickerActive: false
            })
          }
        >
          <Pipette size={14} />
        </button>
      </div>
      <div className="studio-color-target-row">
        <span className="studio-color-target-label">{t('studio.color.editor.target.srgb')}</span>
        <Field
          label={t('studio.color.editor.target.hexAria')}
          className="is-hex"
          disabled={disabled}
          value={hexOf(rgb).toUpperCase()}
          commit={(text) => setTarget(hexLab(text))}
          // Three digits are a shorthand only once typing stops; while typed they would flash another colour.
          preview={(text) => /^\s*#?[0-9a-f]{6}\s*$/i.test(text) && previewTarget(hexLab(text))}
        />
        {rgbField(0, t('studio.color.editor.target.redAria'))}
        {rgbField(1, t('studio.color.editor.target.greenAria'))}
        {rgbField(2, t('studio.color.editor.target.blueAria'))}
      </div>
      <div className="studio-color-target-row">
        <span className="studio-color-target-label">{t('studio.color.editor.target.lch')}</span>
        {lchField(0, t('studio.color.editor.target.lightnessAria'), 100, 1)}
        {lchField(1, t('studio.color.editor.target.chromaAria'), 200, 1)}
        {lchField(2, t('studio.color.editor.target.hueAria'), 360, 0)}
      </div>
      {sampled && (
        <button
          type="button"
          className="studio-color-target-use"
          disabled={disabled}
          title={t('studio.color.editor.target.useSampledTitle', { name: baseName(sampled.sampledIn!) })}
          onClick={() => setTarget(sampled.lab, sampled.sampledIn)}
        >
          <span className="studio-color-target-chip" style={{ background: cssOf(sampled.lab) }} />
          {t('studio.color.editor.target.useSampled', { name: baseName(sampled.sampledIn!) })}
        </button>
      )}
      {own && (
        <p
          className="studio-hint studio-color-target-readout"
          role="status"
          title={t('studio.color.editor.target.readoutTitle')}
        >
          {readout()}
          {solve?.targetOutsideWorkingGamut ? (
            <> {t('studio.color.editor.target.outsideWorkingGamut')}</>
          ) : (
            beyondSrgb &&
            !(measuredDeltaE !== null && deltaE > TARGET_REACHED) && <> {t('studio.color.editor.target.outsideSrgb')}</>
          )}
          {fullStrength && !grey && !(deltaE <= TARGET_REACHED && !pickedThrough) && (
            <> {t('studio.color.editor.target.layerStrength')}</>
          )}
          {sourceClipped && !grey && <> {t('studio.color.editor.target.sourceClipped')}</>}
        </p>
      )}
    </div>
  );
}
