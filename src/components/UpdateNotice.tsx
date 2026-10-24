// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect } from 'react';
import { open } from '@tauri-apps/plugin-shell';
import { useTranslation } from 'react-i18next';
import { useUpdateStore } from '../store/useUpdateStore';
import { useSettingsStore } from '../store/useSettingsStore';

export default function UpdateNotice() {
  const { t } = useTranslation();
  const update = useUpdateStore((s) => s.update);
  useEffect(() => { void useUpdateStore.getState().check(false); }, []);
  if (!update) return null;
  const dismiss = async () => {
    useUpdateStore.getState().dismiss();
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (appSettings) await handleSettingsChange({ ...appSettings, dismissedUpdateVersion: update.version });
  };
  return (
    <aside role="status" className="fixed bottom-4 right-4 z-[100] max-w-md rounded-lg border border-border bg-bg-secondary p-4 text-sm text-text-primary shadow-lg">
      <p>{t('updates.available', { version: update.version })}</p>
      {update.appImage && <p className="mt-2 text-text-secondary">{t('updates.appImage')}</p>}
      <div className="mt-3 flex gap-4">
        <button type="button" className="rounded px-3 py-2 hover:bg-bg-tertiary focus-visible:outline" onClick={() => { void open(update.releaseUrl).catch(console.error); }}>{t('updates.download')}</button>
        <button type="button" className="rounded px-3 py-2 hover:bg-bg-tertiary focus-visible:outline" onClick={() => { void dismiss(); }}>{t('updates.notNow')}</button>
      </div>
    </aside>
  );
}
