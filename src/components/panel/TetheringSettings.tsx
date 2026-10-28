// SPDX-License-Identifier: AGPL-3.0-or-later
import { useTranslation } from 'react-i18next';
import { open } from '@tauri-apps/plugin-shell';
import { ExternalLink, TriangleAlert } from 'lucide-react';
import Switch from '../ui/Switch';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';
import { type AppSettings } from '../ui/AppProperties';

/** The compatibility page: which cameras work and how to report one. */
export const TETHER_CAMERAS_URL = 'https://github.com/rawtonic-app/rawtonic/blob/main/docs/TETHER_CAMERAS.md';

/** Preferences > Tethering: the opt-in for Canon cameras, which no body has
 * been tested with yet (user decision 2026-10-28). Off by default; with it
 * on a Canon body connects only with Save to on a memory card that is in
 * the camera, and nothing is ever freed on it. */
export default function TetheringSettings({
  appSettings,
  onSettingsChange,
  openUrl = open,
}: {
  appSettings: AppSettings;
  onSettingsChange(settings: AppSettings): void | Promise<void>;
  /** Tests replace the browser. */
  openUrl?(url: string): Promise<void>;
}) {
  const { t } = useTranslation();
  const on = appSettings.tetherExperimentalCanon === true;
  return (
    <div className="pref-row">
      <div className="pref-text">
        <div className="pref-label">{t('settings.tethering.canon')}</div>
        <p className="pref-description">{t('settings.tethering.canonDesc')}</p>
        <div
          className="mt-2 flex gap-2 rounded border border-yellow-500/40 bg-yellow-500/10 p-3 text-sm text-text-primary"
          role="note"
          data-testid="canon-warning"
        >
          <TriangleAlert size={16} className="mt-0.5 shrink-0 text-yellow-400" aria-hidden />
          <div className="flex flex-col gap-1">
            <Text>{t('settings.tethering.warningUntested')}</Text>
            <Text>{t('settings.tethering.warningCard')}</Text>
            <Text>{t('settings.tethering.warningNothingDeleted')}</Text>
          </div>
        </div>
        <button
          type="button"
          className="mt-2 inline-flex items-center gap-1 text-sm underline"
          onClick={() => void openUrl(TETHER_CAMERAS_URL).catch(console.error)}
        >
          {t('settings.tethering.camerasLink')}
          <ExternalLink size={12} aria-hidden />
        </button>
        <Text variant={TextVariants.small} color="secondary">
          {t('settings.tethering.howToTest')}
        </Text>
      </div>
      <div className="pref-control">
        <Switch
          checked={on}
          id="tether-experimental-canon"
          label={t('settings.tethering.canonSwitch')}
          onChange={(checked) => void onSettingsChange({ ...appSettings, tetherExperimentalCanon: checked })}
        />
      </div>
    </div>
  );
}
