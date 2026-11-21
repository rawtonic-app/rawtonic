// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { EyeClosed, FileText, ImageOff, Layers, ListFilter } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';
import { ToolGroup } from './StudioDevelop';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { CRITERIA, cullCounts, dismissSuggestion, useCullStore } from '../../store/useCullStore';
import { useSessionBar } from '../session/sessionBar';
import { useAutoStackDialog } from '../../utils/stacks';
import { imageFlag } from '../../utils/culling';
import { downloadSize } from '../../utils/contentSearch';
import { downloadSearchModel, refreshContentStatus } from '../../hooks/useContentSearch';
import { checkEyes } from '../../hooks/useEyeCheck';
import { notify } from '../../activity/notify';
import { errorMessage } from '../../shared/ipc/errors';
import type { CullCriterion, ImageFile } from '../ui/AppProperties';

/**
 * Assisted culling in one place (G35, ADOPT2 §8; notes: benchmark-results/
 * cul1-2026-11-21/REPORT.md): misfires and documents (from the search
 * model's reading of each photograph) and closed eyes (the eye check), each
 * with its count, a filter and a tag on the photograph. Suggestions only:
 * Reject flags the checked suggestions as rejected in one undo step when the
 * photographer clicks it; nothing is rejected, hidden or removed by itself.
 * "Not a problem" dismisses a suggestion for the selected photographs for
 * good; Show again brings it back. Auto-Stack groups by capture time and
 * look (`AutoStackDialog`).
 */

const ICONS: Record<CullCriterion, typeof ImageOff> = {
  misfire: ImageOff,
  document: FileText,
  eyesClosed: EyeClosed
};

/** Criteria Reject takes (remembered); closed eyes is a "check these" list
 * by default (CUL1: precision 0.24, intentional blinks happen). */
const DEFAULT_CHECKED: CullCriterion[] = ['misfire', 'document'];
const REMEMBERED = 'rawtonic.culling.checked';

function rememberedChecked(): CullCriterion[] {
  try {
    const stored = JSON.parse(localStorage.getItem(REMEMBERED) ?? 'null');
    if (Array.isArray(stored)) return CRITERIA.filter((c) => stored.includes(c));
  } catch {
    // A remembered choice is a convenience only.
  }
  return DEFAULT_CHECKED;
}

export function criterionLabel(t: TFunction, criterion: CullCriterion): string {
  switch (criterion) {
    case 'misfire':
      return t('studio.culling.criteria.misfire');
    case 'document':
      return t('studio.culling.criteria.document');
    default:
      return t('studio.culling.criteria.eyesClosed');
  }
}

/** A grid tile's tag: the misfire or document suggestion (closed eyes have
 * their own badge, `PhotoEyesBadge`). Nothing otherwise. */
export function PhotoCullBadge({ path, compact }: { path: string; compact?: boolean }) {
  const { t } = useTranslation();
  const suggested = useCullStore((s) => s.photos[path]?.suggested);
  const shown = suggested?.filter((c) => c !== 'eyesClosed') ?? [];
  if (!shown.length) return null;
  // One photograph's tag names the suggestion in the singular.
  const names = shown
    .map((c) =>
      c === 'misfire'
        ? t('studio.library.smartAlbums.choices.suggestion.misfire')
        : t('studio.library.smartAlbums.choices.suggestion.document')
    )
    .join(' · ');
  const Icon = ICONS[shown[0]];
  return (
    <span
      className="studio-cull-badge"
      role="img"
      aria-label={t('studio.culling.badge', { names })}
      title={`${t('studio.culling.badge', { names })}\n${t('studio.culling.suggestionOnly')}`}
    >
      <Icon size={11} />
      {!compact && <span>{names}</span>}
    </span>
  );
}

/** `shown`: the photos the grid shows (filters, search, collapsed stacks
 * applied); Reject flags only those, never photos the photographer cannot
 * see, and nothing while the loupe or full screen shows one photo (`null`).
 * The counts are the folder's or album's. */
export function StudioCulling({ shown }: { shown?: ImageFile[] | null }) {
  const { t, i18n } = useTranslation();
  const { directory, imageList, imageFlags, selected, suggestion, contentStatus, setFilter } = useLibraryStore(
    useShallow((s) => ({
      directory: s.sessionDirectory,
      imageList: s.imageList,
      imageFlags: s.imageFlags,
      selected: s.multiSelectedPaths,
      suggestion: s.filterCriteria.suggestion ?? 'all',
      contentStatus: s.contentStatus,
      setFilter: s.setFilterCriteria
    }))
  );
  const contentOn = useSettingsStore((s) => !!s.appSettings?.contentSearch);
  const eyeCheck = useSettingsStore((s) => !!s.appSettings?.eyeCheck);
  const photos = useCullStore((s) => s.photos);
  const mark = useSessionBar((s) => s.mark);
  const openAutoStack = useAutoStackDialog((s) => s.open);
  const [checked, setChecked] = useState<CullCriterion[]>(rememberedChecked);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (directory) refreshContentStatus(directory).catch(() => {});
  }, [directory, contentOn]);

  const paths = useMemo(() => imageList.map((image) => image.path), [imageList]);
  const counts = useMemo(() => cullCounts(photos, paths), [photos, paths]);
  const toReject = useMemo(
    () =>
      shown === null
        ? []
        : (shown ?? imageList)
            .map((image) => image.path)
            .filter(
              (path) => imageFlag(imageFlags, path) !== -1 && photos[path]?.suggested.some((c) => checked.includes(c))
            ),
    [shown, imageList, checked, photos, imageFlags]
  );
  const selectedSuggested = selected.filter((p) => photos[p]?.suggested.some((c) => checked.includes(c)));
  const selectedDismissed = selected.filter((p) => photos[p]?.dismissed.length);

  if (!directory) return null;
  const number = (n: number) => n.toLocaleString(i18n.language);

  const toggle = (criterion: CullCriterion) => {
    const next = checked.includes(criterion) ? checked.filter((c) => c !== criterion) : [...checked, criterion];
    setChecked(CRITERIA.filter((c) => next.includes(c)));
    try {
      localStorage.setItem(REMEMBERED, JSON.stringify(next));
    } catch {
      // Kept for this session only.
    }
  };
  const dismiss = async (targets: string[], dismissed: boolean) => {
    setBusy(true);
    try {
      for (const criterion of dismissed ? checked : CRITERIA) {
        const mine = targets.filter((p) =>
          dismissed ? photos[p]?.suggested.includes(criterion) : photos[p]?.dismissed.includes(criterion)
        );
        if (mine.length) await dismissSuggestion(directory, mine, criterion, dismissed);
      }
    } catch (error) {
      notify.error(t('studio.culling.failed', { error: errorMessage(error) }));
    } finally {
      setBusy(false);
    }
  };

  const reading = contentOn && contentStatus?.installed;
  let modelLine = null;
  if (!reading) {
    const size = contentStatus && !contentStatus.installed ? contentStatus.downloadBytes : 0;
    const label =
      size > 0
        ? t('studio.culling.analyseDownload', { size: downloadSize(size, i18n.language) })
        : t('studio.culling.analyse');
    modelLine = (
      <div className="studio-culling-setup">
        <p className="studio-help">{t('studio.culling.needsModel')}</p>
        <button
          type="button"
          disabled={busy}
          title={t('studio.contentSearch.downloadTitle')}
          onClick={() => {
            setBusy(true);
            void downloadSearchModel(directory).finally(() => setBusy(false));
          }}
        >
          {label}
        </button>
      </div>
    );
  } else if (counts.scored + counts.failed < counts.known) {
    modelLine = (
      <p className="studio-help" role="status">
        {t('studio.culling.reading', { scored: number(counts.scored), total: number(counts.known) })}
      </p>
    );
  } else if (counts.failed > 0) {
    modelLine = (
      <p className="studio-help" role="status">
        {t('studio.culling.unreadable', { count: counts.failed, n: number(counts.failed) })}
      </p>
    );
  }

  return (
    <ToolGroup title="Culling" label={t('studio.culling.title')} before={false}>
      <div className="studio-culling">
        {modelLine}
        <ul className="studio-culling-criteria" aria-label={t('studio.culling.criteriaLabel')}>
          {CRITERIA.map((criterion) => {
            const Icon = ICONS[criterion];
            const name = criterionLabel(t, criterion);
            const count = counts.suggested[criterion].length;
            const filtering = suggestion === criterion;
            return (
              <li key={criterion} className="studio-culling-criterion">
                <label title={t('studio.culling.checkTitle')}>
                  <input type="checkbox" checked={checked.includes(criterion)} onChange={() => toggle(criterion)} />
                  <Icon size={12} aria-hidden="true" />
                  <span>{name}</span>
                </label>
                {criterion === 'eyesClosed' && !eyeCheck ? (
                  <button type="button" className="studio-culling-eyes" onClick={() => void checkEyes(directory)}>
                    {t('studio.culling.checkEyes')}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="studio-culling-count"
                    aria-pressed={filtering}
                    aria-label={t('studio.culling.showOnly', { name, n: number(count) })}
                    title={t('studio.culling.showOnly', { name, n: number(count) })}
                    onClick={() => setFilter({ suggestion: filtering ? 'all' : criterion })}
                  >
                    <ListFilter size={11} aria-hidden="true" />
                    <span>{number(count)}</span>
                  </button>
                )}
              </li>
            );
          })}
        </ul>
        <button
          type="button"
          className="studio-culling-reject"
          disabled={busy || !mark || toReject.length === 0}
          title={t('studio.culling.rejectTitle')}
          onClick={() => void mark?.(-1, toReject)}
        >
          {t('studio.culling.reject', { count: toReject.length, n: number(toReject.length) })}
        </button>
        {shown === null && <p className="studio-help">{t('studio.culling.gridOnly')}</p>}
        {(selectedSuggested.length > 0 || selectedDismissed.length > 0) && (
          <div className="studio-culling-selection">
            {selectedSuggested.length > 0 && (
              <button
                type="button"
                disabled={busy}
                title={t('studio.culling.dismissTitle')}
                onClick={() => void dismiss(selectedSuggested, true)}
              >
                {t('studio.culling.dismiss', { count: selectedSuggested.length, n: number(selectedSuggested.length) })}
              </button>
            )}
            {selectedDismissed.length > 0 && (
              <button type="button" disabled={busy} onClick={() => void dismiss(selectedDismissed, false)}>
                {t('studio.culling.showAgain', {
                  count: selectedDismissed.length,
                  n: number(selectedDismissed.length)
                })}
              </button>
            )}
          </div>
        )}
        <button
          type="button"
          className="studio-culling-stack"
          disabled={paths.length < 2}
          onClick={() => openAutoStack(paths)}
        >
          <Layers size={12} aria-hidden="true" />
          <span>{t('studio.culling.autoStack')}</span>
        </button>
        <p className="studio-help">{t('studio.culling.suggestionOnly')}</p>
      </div>
    </ToolGroup>
  );
}
