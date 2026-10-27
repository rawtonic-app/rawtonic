// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useState } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { useTranslation } from 'react-i18next';
import Switch from '../ui/Switch';
import { type AppSettings } from '../ui/AppProperties';
import { useUpdateStore } from '../../store/useUpdateStore';

export default function UpdateSettings({ appSettings, onSettingsChange }: {
  appSettings: AppSettings;
  onSettingsChange(settings: AppSettings): void | Promise<void>;
}) {
  const { t } = useTranslation();
  const [prereleasesDefault, setPrereleasesDefault] = useState<boolean | null>(null);
  useEffect(() => {
    void getVersion()
      .then((version) => setPrereleasesDefault(version.split('+')[0].includes('-')))
      .catch(console.debug);
  }, []);
  const checking = useUpdateStore((s) => s.checking);
  const checked = useUpdateStore((s) => s.checked);
  const failed = useUpdateStore((s) => s.failed);
  const update = useUpdateStore((s) => s.update);
  return (
    <div className="space-y-3">
      <Switch
        checked={appSettings.checkForUpdates ?? true}
        label={t('updates.startup')}
        onChange={(checked) => { void onSettingsChange({ ...appSettings, checkForUpdates: checked }); }}
      />
      <Switch
        disabled={prereleasesDefault === null && appSettings.updatePrereleases === undefined}
        checked={appSettings.updatePrereleases ?? prereleasesDefault ?? false}
        label={t('updates.prereleases')}
        onChange={(checked) => { void onSettingsChange({ ...appSettings, updatePrereleases: checked }); }}
      />
      <Switch
        checked={appSettings.showWhatsNew ?? true}
        label={t('updates.showWhatsNew')}
        onChange={(checked) => { void onSettingsChange({ ...appSettings, showWhatsNew: checked }); }}
      />
      <p className="text-sm text-text-secondary">{t('updates.privacy')}</p>
      <button
        type="button"
        disabled={checking}
        className="rounded border border-border px-3 py-2 text-sm hover:bg-bg-tertiary disabled:opacity-50"
        onClick={() => { void useUpdateStore.getState().check(true); }}
      >
        {checking ? t('updates.checking') : t('updates.checkNow')}
      </button>
      {failed && <p role="status" className="text-sm text-text-secondary">{t('updates.failed')}</p>}
      {checked && !update && <p role="status" className="text-sm text-text-secondary">{t('updates.noUpdate')}</p>}
    </div>
  );
}
