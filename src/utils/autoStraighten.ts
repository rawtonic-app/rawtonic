import i18next from 'i18next';
import { toast } from 'react-toastify';
import type { StraightenReport } from '../hooks/useEditorActions';

/** Runs Auto straighten (button or shortcut) and says so when the photo has
 * no straight lines to level by; an answer for a photo or frame changed
 * meanwhile is dropped silently. */
export async function runAutoStraighten(action: () => Promise<'done' | 'none' | 'stale'>): Promise<void> {
  try {
    if ((await action()) === 'none') toast.info(i18next.t('studio.crop.autoStraightenNoLines'));
  } catch (err) {
    toast.error(i18next.t('studio.crop.autoStraightenFailed', { error: String(err) }));
  }
}

/** Runs Auto straighten for a selection and sums up what it did. */
export async function runAutoStraightenPaths(
  action: (paths?: string[]) => Promise<StraightenReport | null>,
  paths: string[],
): Promise<void> {
  try {
    const report = await action(paths);
    if (!report) return;
    const counts = {
      straightened: report.straightened.length,
      noLines: report.noLines.length,
      changed: report.changed.length,
      failed: report.failed.length,
    };
    const message = i18next.t('studio.crop.autoStraightenBatch', counts);
    if (counts.failed > 0) toast.error(`${message} ${report.failed.map(([p, e]) => `${p.split(/[\\/]/).pop()}: ${e}`).join('; ')}`);
    else if (counts.noLines + counts.changed > 0) toast.info(message);
    else toast.success(message);
  } catch (err) {
    toast.error(i18next.t('studio.crop.autoStraightenFailed', { error: String(err) }));
  }
}
