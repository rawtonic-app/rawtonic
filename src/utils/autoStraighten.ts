import i18next from 'i18next';
import { toast } from 'react-toastify';

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
