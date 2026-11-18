// SPDX-License-Identifier: AGPL-3.0-or-later
import { type ReactNode, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-shell';
import { useTranslation } from 'react-i18next';
import { type InstallState, useUpdateStore } from '../store/useUpdateStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes } from './ui/AppProperties';
import { ReleaseNotesDialog } from './ReleaseNotes';

/** The message per failure; a cancelled download goes back to the offer. */
const FAILURE_TEXT = {
  network: 'updates.failure.network',
  signature: 'updates.failure.signature',
  noPackage: 'updates.failure.noPackage',
  disk: 'updates.failure.disk',
  permission: 'updates.failure.permission',
  changed: 'updates.failure.changed',
  notOffered: 'updates.failure.notOffered',
  stopped: 'updates.failure.other',
  other: 'updates.failure.other'
} as const;

const megabytes = (bytes: number, language: string) =>
  new Intl.NumberFormat(language, { maximumFractionDigits: 0 }).format(bytes / 1e6);

export default function UpdateNotice() {
  const { t, i18n } = useTranslation();
  const update = useUpdateStore((s) => s.update);
  const install = useUpdateStore((s) => s.install);
  const installVersion = useUpdateStore((s) => s.installVersion);
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
  // file fits this computer).
  const target = update.downloadUrl ?? update.releaseUrl;
  const openInBrowser = () => {
    void open(target).catch(console.error);
  };
  const { download, cancelDownload, restart } = useUpdateStore.getState();
  // The in-app update belongs to the version it was started for.
  const state: InstallState = installVersion === update.version ? install : { phase: 'idle' };
  const notesButton = (
    <button type="button" aria-haspopup="dialog" onClick={() => setNotesOpen(true)}>
      {t('updates.whatsNew')}
    </button>
  );
  const later = (persist: boolean) => (
    <button
      type="button"
      className="app-notice-later"
      onClick={() => {
        if (persist) void dismiss();
        else useUpdateStore.getState().dismiss();
      }}
    >
      {persist ? t('updates.notNow') : t('updates.later')}
    </button>
  );
  let title = t('updates.available', { version: update.version });
  let body: ReactNode;
  let actions: ReactNode;
  switch (state.phase) {
    case 'downloading': {
      const { downloaded, total } = state;
      body = (
        <>
          <p className="app-notice-detail" aria-live="polite">
            {total
              ? t('updates.downloadingOf', {
                  percent: Math.min(100, Math.floor((downloaded / total) * 100)),
                  done: megabytes(downloaded, i18n.language),
                  size: megabytes(total, i18n.language)
                })
              : t('updates.downloading', { done: megabytes(downloaded, i18n.language) })}
          </p>
          <progress className="app-notice-progress" max={total ?? undefined} value={total ? downloaded : undefined} />
        </>
      );
      actions = (
        <button
          type="button"
          onClick={() => {
            void cancelDownload();
          }}
        >
          {t('updates.cancel')}
        </button>
      );
      break;
    }
    case 'ready':
      title = t('updates.ready', { version: update.version });
      body = <p className="app-notice-detail">{t('updates.readyDetail')}</p>;
      actions = (
        <>
          <button
            type="button"
            className="is-primary"
            onClick={() => {
              void restart();
            }}
          >
            {t('updates.restartNow')}
          </button>
          {notesButton}
          {later(false)}
        </>
      );
      break;
    case 'installing':
      title = t('updates.ready', { version: update.version });
      body = (
        <p className="app-notice-detail" aria-live="polite">
          {t('updates.installing')}
        </p>
      );
      actions = null;
      break;
    case 'failed': {
      const { failure, during } = state;
      body = (
        <p className="app-notice-detail" role="alert">
          {failure.kind === 'unsaved'
            ? t('updates.failure.unsaved', { changes: failure.items.join(', ') })
            : failure.kind === 'notRestored'
              ? t('updates.failure.notRestored', { path: failure.path })
              : t(FAILURE_TEXT[failure.kind])}
        </p>
      );
      // The verified package is kept after a failed install (not after a
      // failed restore): try again from it, else download again.
      const retry = () => {
        if (during === 'install' && failure.kind !== 'notRestored') {
          useUpdateStore.setState({ install: { phase: 'ready' } });
          void restart();
        } else void download();
      };
      actions = (
        <>
          {failure.kind !== 'noPackage' && failure.kind !== 'notOffered' && (
            <button type="button" className="is-primary" onClick={retry}>
              {t('updates.tryAgain')}
            </button>
          )}
          <button type="button" onClick={openInBrowser}>
            {update.downloadUrl ? t('updates.downloadInstead') : t('updates.releasePage')}
          </button>
          {later(false)}
        </>
      );
      break;
    }
    default:
      body = update.inApp ? (
        <p className="app-notice-detail">{t('updates.inAppDetail')}</p>
      ) : (
        <>
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
          {(update.fallback === 'allUsers' || update.fallback === 'moveToApplications') && (
            <p className="app-notice-detail">
              {update.fallback === 'allUsers'
                ? t('updates.fallback.allUsers')
                : t('updates.fallback.moveToApplications')}
            </p>
          )}
          {update.appImage && <p className="app-notice-detail">{t('updates.appImage')}</p>}
        </>
      );
      actions = (
        <>
          {update.inApp ? (
            <button
              type="button"
              className="is-primary"
              onClick={() => {
                void download();
              }}
            >
              {t('updates.install')}
            </button>
          ) : (
            <button type="button" className="is-primary" onClick={openInBrowser}>
              {update.downloadUrl ? t('updates.download') : t('updates.releasePage')}
            </button>
          )}
          {notesButton}
          {later(true)}
        </>
      );
  }
  return (
    <>
      <aside role="status" aria-labelledby="app-update-title" className="app-notice app-update-notice">
        <div className="app-notice-body">
          <p id="app-update-title" className="app-notice-title">
            {title}
          </p>
          {body}
          {actions && <div className="app-notice-actions">{actions}</div>}
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
