// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useState, type ReactNode } from 'react';
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
  // One row per preference: its name and explanation left, its control right.
  const row = (label: string, control: ReactNode, description?: ReactNode) => (
    <div className="pref-row">
      <div className="pref-text">
        <div className="pref-label">{label}</div>
        {description}
      </div>
      <div className="pref-control">{control}</div>
    </div>
  );
  return (
    <>
      {row(
        t('updates.startup'),
        <Switch
          checked={appSettings.checkForUpdates ?? true}
          label={t('updates.startup')}
          onChange={(checked) => { void onSettingsChange({ ...appSettings, checkForUpdates: checked }); }}
        />,
        <p className="pref-description">{t('updates.privacy')}</p>,
      )}
      {row(
        t('updates.prereleases'),
        <Switch
          disabled={prereleasesDefault === null && appSettings.updatePrereleases === undefined}
          checked={appSettings.updatePrereleases ?? prereleasesDefault ?? false}
          label={t('updates.prereleases')}
          onChange={(checked) => { void onSettingsChange({ ...appSettings, updatePrereleases: checked }); }}
        />,
      )}
      {row(
        t('updates.showWhatsNew'),
        <Switch
          checked={appSettings.showWhatsNew ?? true}
          label={t('updates.showWhatsNew')}
          onChange={(checked) => { void onSettingsChange({ ...appSettings, showWhatsNew: checked }); }}
        />,
      )}
      {row(
        t('updates.manual'),
        <button
          type="button"
          disabled={checking}
          className="studio-button"
          onClick={() => { void useUpdateStore.getState().check(true); }}
        >
          {checking ? t('updates.checking') : t('updates.checkNow')}
        </button>,
        <>
          {failed && <p role="status" className="pref-description">{t('updates.failed')}</p>}
          {checked && !update && <p role="status" className="pref-description">{t('updates.noUpdate')}</p>}
        </>,
      )}
    </>
  );
}
