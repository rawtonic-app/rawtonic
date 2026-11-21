import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { useEditorStore } from '../../store/useEditorStore';
import { getTransformAdjustments } from '../../hooks/useAiMasking';
import { Invokes } from '../ui/AppProperties';
import type { SubMask } from '../panel/right/Masks';
import { LANDSCAPE_PARTS, type LandscapePart } from '../../utils/maskKinds';
import { aiErrorMessage } from '../../utils/aiErrors';
import AdjustmentControl from './AdjustmentControl';

/** Share of the photo below which a part counts as not found (0.5 %). */
export const LANDSCAPE_FOUND_SHARE = 0.005;

interface Props {
  sub: SubMask;
  setParameters(patch: Record<string, unknown>): void;
  onInteraction(dragging: boolean): void;
}

/**
 * Lightroom's Landscape mask: which parts of the landscape (mountains,
 * water, …) the layer selects, as one mask. The parts the photo has are
 * marked; the others stay selectable (the network may miss a part the
 * photographer sees). Changing the parts computes the mask again.
 */
export default function StudioLandscapeMask({ sub, setParameters, onInteraction }: Props) {
  const { t } = useTranslation();
  const path = useEditorStore((s) => s.selectedImage?.path);
  const adjustments = useEditorStore((s) => s.adjustments);
  const geometry = useMemo(() => getTransformAdjustments(adjustments), [adjustments]);
  const geometryKey = JSON.stringify(geometry);
  const [shares, setShares] = useState<Record<string, number> | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!path) return;
    let current = true;
    setShares(null);
    setError(null);
    invoke<[string, number][]>(Invokes.DetectLandscapeParts, { jsAdjustments: JSON.parse(geometryKey), path })
      .then((found) => current && setShares(Object.fromEntries(found)))
      .catch((e) => current && setError(aiErrorMessage(e)));
    return () => {
      current = false;
    };
  }, [path, geometryKey]);

  const parts = ((sub.parameters as { parts?: LandscapePart[] }).parts ?? []).filter((p) =>
    LANDSCAPE_PARTS.includes(p)
  );
  const found = (part: LandscapePart) => (shares?.[part] ?? 0) >= LANDSCAPE_FOUND_SHARE;
  const toggle = (part: LandscapePart) =>
    setParameters({
      // Kept in the list's order, so the same parts are the same recipe.
      parts: LANDSCAPE_PARTS.filter((p) => (p === part ? !parts.includes(p) : parts.includes(p))),
      maskDataBase64: null
    });

  return (
    <div className="studio-people" role="group" aria-label={t('studio.landscape.aria')}>
      {shares === null && !error && <p className="studio-hint">{t('studio.landscape.finding')}</p>}
      {shares !== null && !LANDSCAPE_PARTS.some(found) && (
        <p className="studio-hint" role="note">
          {t('studio.landscape.noneFound')}
        </p>
      )}
      {error && (
        <p className="studio-hint" role="alert">
          {error}
        </p>
      )}
      {parts.length === 0 && shares !== null && (
        <p className="studio-hint" role="note">
          {t('studio.landscape.choose')}
        </p>
      )}
      <div className="studio-people-parts" role="group" aria-label={t('studio.landscape.partsAria')}>
        {LANDSCAPE_PARTS.map((part) => (
          <label key={part} className="studio-check" data-found={shares === null ? undefined : found(part)}>
            <input type="checkbox" checked={parts.includes(part)} onChange={() => toggle(part)} />
            {t(`studio.landscape.parts.${part}`)}
            {shares !== null && !found(part) && (
              <small className="studio-hint"> {t('studio.landscape.notFound')}</small>
            )}
          </label>
        ))}
      </div>
      <AdjustmentControl
        label={t('editor.masks.params.grow')}
        min={-100}
        max={100}
        value={Number(sub.parameters.grow ?? 0)}
        defaultValue={0}
        onInteraction={onInteraction}
        onChange={(grow) => setParameters({ grow })}
      />
      <AdjustmentControl
        label={t('editor.masks.params.feather')}
        min={0}
        max={100}
        value={Number(sub.parameters.feather ?? 0)}
        defaultValue={0}
        onInteraction={onInteraction}
        onChange={(feather) => setParameters({ feather })}
      />
    </div>
  );
}
