// SPDX-License-Identifier: AGPL-3.0-or-later
import { useTranslation } from 'react-i18next';
import Switch from '../ui/Switch';
import { type AppSettings } from '../ui/AppProperties';
import { setCrashReportsEnabled } from '../contribute/reports';

/** Whether a crash is recorded and offered as a report on the next start
 * (ADR 0017); turning it off deletes the records kept so far and stops
 * recording at once. */
export default function CrashReportSettings({ appSettings, onSettingsChange }: {
  appSettings: AppSettings;
  onSettingsChange(settings: AppSettings): void | Promise<void>;
}) {
  const { t } = useTranslation();
  return (
    <Switch
      checked={appSettings.crashReports ?? true}
      label={t('contribute.crash.setting')}
      onChange={(checked) => {
        void onSettingsChange({ ...appSettings, crashReports: checked });
        void setCrashReportsEnabled(checked).catch(console.error);
      }}
    />
  );
}
