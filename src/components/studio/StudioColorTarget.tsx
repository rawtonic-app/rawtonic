import { Pipette } from 'lucide-react';
import { useEffect, useRef, useState, type KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { useShallow } from 'zustand/react/shallow';
import { useEditorStore } from '../../store/useEditorStore';
import type { ColorRange } from '../../utils/adjustments';
import { rangeInputFor, rangePickKey, rangePickOf, rangesWithTarget, targetPickFor } from '../../utils/colorRanges';
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
  solveColorTarget,
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

/** A field that edits on Return or when left; Escape restores it. Return
 * and Escape leave the field, so the app's shortcuts (Undo) work again. */
function Field({
  value,
  label,
  className,
  disabled,
  commit
}: {
  value: string;
  label: string;
  className?: string;
  disabled: boolean;
  /** False when the text is not a valid value (the field then reverts). */
  commit(text: string): boolean;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  // The text being edited, read when the field is left: a blur that follows
  // a key in the same event sees it before React re-renders.
  const editing = useRef<string | null>(null);
  const edit = (text: string | null) => {
    editing.current = text;
    setDraft(text);
  };
  const finish = () => {
    const text = editing.current;
    edit(null);
    if (text !== null && text.trim() !== value) commit(text);
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
        if (e.key === 'Enter') {
          finish();
          e.currentTarget.blur();
        } else if (e.key === 'Escape') {
          edit(null);
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
  const { target, photo, pickerTarget, through, setEditor } = useEditorStore(
    useShallow((s) => ({
      target: s.colorTarget,
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
  const latestRanges = useRef(ranges);
  latestRanges.current = ranges;
  const latestActive = useRef(active);
  latestActive.current = active;
  // The latest target request: an older one's sample arriving later is dropped.
  const request = useRef(0);
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
  const input = pick?.input ?? centreInput(active);
  // Edits before the colour editor or of the layer's mask change what it
  // receives at the pick: sample it again once they settle, so the readout
  // follows them.
  const followed = !!own && !!pick;
  useEffect(() => {
    if (!photo || !followed) return;
    const timer = setTimeout(() => void rangeInputFor(photo, layer, latestActive.current), 250);
    return () => clearTimeout(timer);
  }, [adjustments, photo, layer, active.id, followed]);
  // The colour at the pick as the renderer leaves it (the sliders as they are).
  const resultLab = renderedLab(ranges, input);
  const shown = own ? own.lab : resultLab;
  const shownLinear = labToLinear(shown);
  const rgb = linearToSrgb8(shownLinear);
  const [L, C, h] = labToLch(shown);

  const setTarget = (lab: Vec3, sampledIn: string | null = null) => {
    if (!photo || !lab.every(Number.isFinite)) return false;
    setEditor({ colorTarget: { lab, sampledIn, appliedTo: { photo, layer, rangeId: active.id } } });
    const mine = ++request.current;
    if (!rangePickOf(photo, layer, active)) {
      apply(rangesWithTarget(ranges, active.id, lab));
      return true;
    }
    // Picked in the photo: solve on the renderer's sample there, taken again
    // (one edit once it is back; nothing when the photo or range changed).
    const id = active.id;
    void rangeInputFor(photo, layer, active).then((fresh) => {
      const state = useEditorStore.getState();
      const applied = state.colorTarget?.appliedTo;
      if (mine !== request.current) return;
      if (state.selectedImage?.path !== photo || applied?.rangeId !== id || applied.layer !== layer) return;
      // The ranges as they are now (another edit may have come meanwhile).
      const current = latestRanges.current;
      if (current.some((r) => r.id === id)) apply(rangesWithTarget(current, id, lab, fresh));
    });
    return true;
  };
  const number = (text: string) => {
    const v = Number(text.replace(',', '.'));
    return text.trim() !== '' && Number.isFinite(v) ? v : null;
  };
  const rgbField = (i: 0 | 1 | 2, label: string) => (
    <Field
      key={label}
      label={label}
      disabled={disabled}
      value={String(Math.round(rgb[i]))}
      commit={(text) => {
        const v = number(text);
        if (v === null || v < 0 || v > 255) return false;
        const next = [...rgb] as Vec3;
        next[i] = v;
        return setTarget(linearToLab(srgb8ToLinear(next)));
      }}
    />
  );
  const lchField = (i: 0 | 1 | 2, label: string, max: number, digits: number) => (
    <Field
      key={label}
      label={label}
      disabled={disabled}
      value={[L, C, h][i].toFixed(digits)}
      commit={(text) => {
        const v = number(text);
        if (v === null || v < 0 || v > max) return false;
        const next: Vec3 = [L, C, h];
        next[i] = i === 2 ? v % 360 : v;
        return setTarget(lchToLab(next));
      }}
    />
  );

  const solve = own ? solveColorTarget(ranges, active.id, own.lab, input) : null;
  const deltaE = own ? deltaE00(own.lab, resultLab) : 0;
  const limits = solve
    ? (['hue', 'saturation', 'lightness'] as const)
        .filter((k) => solve.limited[k])
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
  const sourceClipped = !pick && linearToSrgb8(oklabToLinear(centre)).some((v) => v <= 1 || v >= 254);
  // A layer's prediction is at full strength without the mask influence at the pick.
  const fullStrength = !!layerId && !pick;
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
            result: hexOf(linearToSrgb8(labToLinear(resultLab))),
            target: hexOf(rgb)
          })}
          style={{
            background: `linear-gradient(90deg, ${cssOf(resultLab)} 50%, ${cssOf(shown)} 50%)`
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
          commit={(text) => {
            const parsed = parseHex(text);
            return !!parsed && setTarget(linearToLab(srgb8ToLinear(parsed)));
          }}
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
          {solve?.uncovered
            ? t('studio.color.editor.target.uncovered')
            : grey
              ? t('studio.color.editor.target.grey')
              : pickedThrough
                ? t('studio.color.editor.target.pickedThrough', { value })
                : deltaE <= TARGET_REACHED
                  ? fullStrength
                    ? t('studio.color.editor.target.layerPredicted', { value })
                    : t('studio.color.editor.target.reached', { value })
                  : limits.length
                    ? t('studio.color.editor.target.outOfReach', { value, limits: limits.join(', ') })
                    : t('studio.color.editor.target.distance', { value })}
          {solve?.targetOutsideWorkingGamut ? (
            <> {t('studio.color.editor.target.outsideWorkingGamut')}</>
          ) : (
            !insideSrgb(shownLinear) && <> {t('studio.color.editor.target.outsideSrgb')}</>
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
