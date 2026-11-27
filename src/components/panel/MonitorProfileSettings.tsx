// SPDX-License-Identifier: AGPL-3.0-or-later
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import Button from '../ui/Button';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';
import { Invokes } from '../ui/AppProperties';
import { openDialog } from '../../utils/fileDialog';

/** How the preview reaches the monitor (src-tauri/src/monitor_profile.rs `MonitorStatus`). */
export interface MonitorStatus {
  mode: 'system' | 'converted' | 'srgb' | 'notApplied';
  profile: string | null;
  location: string | null;
  fromOverride: boolean;
  notice: string | null;
  overrideAvailable: boolean;
}

const loadStatusFromApp = () => invoke<MonitorStatus>(Invokes.MonitorProfileStatus);
const chooseInApp = (path: string | null) => invoke<MonitorStatus>(Invokes.MonitorProfileChoose, { path });
const pickProfile = async (title: string) => {
  const picked = await openDialog({
    title,
    purpose: 'monitor-profile',
    multiple: false,
    filters: [{ name: 'ICC', extensions: ['icc', 'icm'] }]
  });
  return typeof picked === 'string' ? picked : null;
};

/** Preferences > Interface > Display colour: which monitor profile the
 * preview is shown through (the system's),
 * and on Windows an ICC profile chosen instead. */
export default function MonitorProfileSettings({
  status: loadStatus = loadStatusFromApp,
  choose = chooseInApp,
  pick = pickProfile
}: {
  /** Tests replace the app's calls. */
  status?(): Promise<MonitorStatus>;
  choose?(path: string | null): Promise<MonitorStatus>;
  pick?(title: string): Promise<string | null>;
}) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<MonitorStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    loadStatus()
      .then((loaded) => setStatus(typeof loaded?.mode === 'string' ? loaded : null))
      .catch(() => setStatus(null));
  }, [loadStatus]);

  useEffect(() => {
    load();
    let unlisten: (() => void) | undefined;
    let gone = false;
    try {
      listen<MonitorStatus>('monitor-profile-changed', (event) => setStatus(event.payload))
        .then((stop) => (gone ? stop() : (unlisten = stop)))
        .catch(() => undefined);
    } catch {
      // No app events (tests, a page outside the app): the status stays as loaded.
    }
    return () => {
      gone = true;
      unlisten?.();
    };
  }, [load]);

  const run = (path: string | null) => {
    setError(null);
    choose(path)
      .then(setStatus)
      .catch((reason: unknown) => setError(String(reason)));
  };

  const name = status?.profile ?? status?.location ?? '';
  const summary = !status
    ? null
    : status.mode === 'system'
      ? t('settings.monitorProfile.system')
      : status.mode === 'converted'
        ? t('settings.monitorProfile.converted', { profile: name })
        : status.mode === 'notApplied'
          ? t('settings.monitorProfile.notApplied', { profile: name })
          : t('settings.monitorProfile.srgb');

  return (
    <div className="pref-row">
      <div className="pref-text">
        <div className="pref-label">{t('settings.monitorProfile.title')}</div>
        <p className="pref-description">{t('settings.monitorProfile.description')}</p>
        {summary && (
          <Text variant={TextVariants.small} data-testid="monitor-profile-summary">
            {summary}
          </Text>
        )}
        {status?.fromOverride && (
          <Text variant={TextVariants.small}>{t('settings.monitorProfile.chosen', { path: status.location })}</Text>
        )}
        {status?.notice && (
          <Text variant={TextVariants.small} color="info" role="status">
            {status.notice}
          </Text>
        )}
        {error && (
          <Text variant={TextVariants.small} color="error" role="alert">
            {t('settings.monitorProfile.refused', { reason: error })}
          </Text>
        )}
      </div>
      {status?.overrideAvailable && (
        <div className="pref-control flex gap-2">
          <Button
            onClick={() => {
              void pick(t('settings.monitorProfile.pickTitle')).then((path) => path && run(path));
            }}
          >
            {t('settings.monitorProfile.choose')}
          </Button>
          {status.fromOverride && <Button onClick={() => run(null)}>{t('settings.monitorProfile.useSystem')}</Button>}
        </div>
      )}
    </div>
  );
}
