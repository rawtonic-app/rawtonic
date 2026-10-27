// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useRef, useState } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { useTranslation } from 'react-i18next';
import { useSettingsStore } from '../store/useSettingsStore';
import { changelogSection, isNewerVersion } from '../utils/changelog';
import { ReleaseNotesDialog } from './ReleaseNotes';

/** On the first start of a newer version, its section of the CHANGELOG.md
 * bundled with the app (no network). A first install only records the
 * version, as does a version that has no section. */
export default function WhatsNewAfterUpdate() {
  const { t } = useTranslation();
  const ready = useSettingsStore((s) => s.appSettings !== null);
  const started = useRef(false);
  const [shown, setShown] = useState<{ version: string; notes: string } | null>(null);
  const [dontShow, setDontShow] = useState(false);

  useEffect(() => {
    if (!ready || started.current) return;
    started.current = true;
    void (async () => {
      const current = await getVersion();
      const { appSettings, handleSettingsChange } = useSettingsStore.getState();
      if (!appSettings || appSettings.lastRunVersion === current) return;
      const last = appSettings.lastRunVersion;
      await handleSettingsChange({ ...appSettings, lastRunVersion: current });
      if (!last || !isNewerVersion(current, last) || appSettings.showWhatsNew === false) return;
      const { default: changelog } = await import('../../CHANGELOG.md?raw');
      const notes = changelogSection(changelog, current);
      if (notes) setShown({ version: current, notes });
    })().catch(console.debug);
  }, [ready]);

  if (!shown) return null;
  const close = () => {
    setShown(null);
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (dontShow && appSettings) void handleSettingsChange({ ...appSettings, showWhatsNew: false });
  };
  return (
    <ReleaseNotesDialog
      title={t('updates.notesTitle', { version: shown.version })}
      notes={shown.notes}
      onClose={close}
      footer={
        <label className="app-notes-optout">
          <input type="checkbox" checked={dontShow} onChange={(e) => setDontShow(e.target.checked)} />
          {t('updates.dontShowAgain')}
        </label>
      }
    />
  );
}
