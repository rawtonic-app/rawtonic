// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-shell';
import { useTranslation } from 'react-i18next';
import { useUpdateStore } from '../store/useUpdateStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes } from './ui/AppProperties';
import { ReleaseNotesDialog } from './ReleaseNotes';

const megabytes = (bytes: number, language: string) =>
  new Intl.NumberFormat(language, { maximumFractionDigits: 0 }).format(bytes / 1e6);

export default function UpdateNotice() {
  const { t, i18n } = useTranslation();
  const update = useUpdateStore((s) => s.update);
  const settingsReady = useSettingsStore((s) => s.appSettings !== null);
  const [notesOpen, setNotesOpen] = useState(false);
  // Windows copies no installer introduced (portable zip, silent install) show
  // the privacy notice once; the startup check waits until it was shown.
  const [noticePending, setNoticePending] = useState(false);
  useEffect(() => {
    void invoke<boolean>(Invokes.PrivacyNoticePending)
      .catch(() => false)
      .then((pending) => {
        if (pending === true) setNoticePending(true);
        else void useUpdateStore.getState().check(false);
      });
  }, []);
  if (noticePending) {
    const acknowledge = () => {
      void invoke(Invokes.PrivacyNoticeAcknowledge)
        .catch(console.error)
        .then(() => {
          setNoticePending(false);
          void useUpdateStore.getState().check(false);
        });
    };
    return (
      <aside role="dialog" aria-labelledby="privacy-notice-title" className="app-notice app-update-notice">
        <div className="app-notice-body">
          <p id="privacy-notice-title" className="app-notice-title">
            {t('updates.privacyNoticeTitle')}
          </p>
          <p className="app-notice-detail">{t('updates.privacyNoticeBody')}</p>
          <div className="app-notice-actions">
            <button type="button" className="is-primary" onClick={acknowledge}>
              {t('updates.privacyNoticeOk')}
            </button>
            <button
              type="button"
              onClick={() => {
                void open('https://rawtonic.cc/privacy/').catch(console.error);
              }}
            >
              {t('updates.privacyNoticeLink')}
            </button>
          </div>
        </div>
      </aside>
    );
  }
  // Dismissal saves a version in settings; never offer it before those are loaded.
  if (!update || !settingsReady) return null;
  const dismiss = async () => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (!appSettings) return;
    useUpdateStore.getState().dismiss();
    await handleSettingsChange({ ...appSettings, dismissedUpdateVersion: update.version });
  };
  // The browser downloads the installer (or shows the release page when no
  // file fits this computer); Rawtonic downloads nothing itself.
  const target = update.downloadUrl ?? update.releaseUrl;
  return (
    <>
      <aside role="status" aria-labelledby="app-update-title" className="app-notice app-update-notice">
        <div className="app-notice-body">
          <p id="app-update-title" className="app-notice-title">
            {t('updates.available', { version: update.version })}
          </p>
          {update.assetName ? (
            <p className="app-notice-detail" title={update.assetName}>
              {update.assetSize
                ? t('updates.fileWithSize', {
                    file: update.assetName,
                    size: megabytes(update.assetSize, i18n.language)
                  })
                : update.assetName}
            </p>
          ) : (
            <p className="app-notice-detail">{t('updates.noFile')}</p>
          )}
          {update.appImage && <p className="app-notice-detail">{t('updates.appImage')}</p>}
          <div className="app-notice-actions">
            <button
              type="button"
              className="is-primary"
              onClick={() => {
                void open(target).catch(console.error);
              }}
            >
              {update.downloadUrl ? t('updates.download') : t('updates.releasePage')}
            </button>
            <button type="button" aria-haspopup="dialog" onClick={() => setNotesOpen(true)}>
              {t('updates.whatsNew')}
            </button>
            <button
              type="button"
              className="app-notice-later"
              onClick={() => {
                void dismiss();
              }}
            >
              {t('updates.notNow')}
            </button>
          </div>
        </div>
      </aside>
      {notesOpen && (
        <ReleaseNotesDialog
          title={t('updates.notesTitle', { version: update.version })}
          notes={update.notes}
          releaseUrl={update.releaseUrl}
          onClose={() => setNotesOpen(false)}
        />
      )}
    </>
  );
}
