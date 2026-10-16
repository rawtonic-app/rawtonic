import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import Button from '../ui/Button';
import Dropdown, { type OptionItem } from '../ui/Dropdown';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';
import { type AppSettings, Invokes, type PreviewCacheInfo, type PreviewUsage } from '../ui/AppProperties';
import { useLibraryStore } from '../../store/useLibraryStore';
import { formatBytes } from '../../utils/previews';

interface Props {
  appSettings: AppSettings | null;
  onSettingsChange(settings: AppSettings): Promise<void> | void;
}

/** Preferences for Standard and 1:1 previews: size, automatic discarding of
 * 1:1 previews (Lightroom's choices), a size limit for Standard previews and
 * their disk use, with discarding. */
export default function PreviewSettings({ appSettings, onSettingsChange }: Props) {
  const { t } = useTranslation();
  const folder = useLibraryStore((s) => s.sessionDirectory ?? s.currentFolderPath);
  const [info, setInfo] = useState<PreviewCacheInfo | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    invoke<PreviewCacheInfo>(Invokes.PreviewCacheUsage, { folder })
      .then(setInfo)
      .catch(() => setInfo(null));
  }, [folder]);
  useEffect(refresh, [refresh]);

  const sizeOptions: OptionItem<number>[] = [
    { label: t('previews.settings.auto'), value: 0 },
    ...[2048, 2560, 3072, 3840].map((size) => ({ label: `${size} px`, value: size })),
  ];
  const discardOptions: OptionItem<number>[] = [
    { label: t('previews.settings.afterDay'), value: 1 },
    { label: t('previews.settings.afterWeek'), value: 7 },
    { label: t('previews.settings.after30Days'), value: 30 },
    { label: t('previews.settings.never'), value: 0 },
  ];

  const limitOptions: OptionItem<number>[] = [
    ...[2, 5, 10, 20, 50].map((gb) => ({ label: `${gb} GB`, value: gb })),
    { label: t('previews.settings.noLimit'), value: 0 },
  ];

  const change = async (patch: Partial<AppSettings>) => {
    if (!appSettings) return;
    await onSettingsChange({ ...appSettings, ...patch });
    if ('discardFullPreviewsDays' in patch || 'standardPreviewsLimitGb' in patch) {
      // Applied at once (the native side reads the saved preferences).
      await invoke(Invokes.ApplyPreviewLimits, { folder }).catch(() => {});
      refresh();
    }
  };

  const clear = async (session: boolean, fullOnly: boolean) => {
    setBusy(true);
    try {
      await invoke<number>(Invokes.ClearPreviewCache, { folder, session, fullOnly });
    } finally {
      setBusy(false);
      refresh();
    }
  };

  const describe = (usage: PreviewUsage) =>
    t('previews.settings.usage', {
      standardFiles: usage.standardCount,
      standardSize: formatBytes(usage.standardBytes),
      fullFiles: usage.fullCount,
      fullSize: formatBytes(usage.fullBytes),
    });

  const folderRow = (session: boolean) => {
    const entry = session ? info?.session : info?.outside;
    if (!entry) return null;
    const empty = entry.usage.standardCount + entry.usage.fullCount === 0;
    return (
      <div className="pref-row" key={session ? 'session' : 'outside'}>
        <div className="pref-text">
          <div className="pref-label">
            {session ? t('previews.settings.sessionUsage') : t('previews.settings.outsideUsage')}
          </div>
          <p className="pref-description">{describe(entry.usage)}</p>
          <Text as="span" variant={TextVariants.small} className="block font-mono break-all">
            {entry.dir}
          </Text>
        </div>
        <div className="pref-control flex gap-2">
          <Button disabled={busy || entry.usage.fullCount === 0} onClick={() => void clear(session, true)}>
            {t('previews.settings.discardFullButton')}
          </Button>
          <Button disabled={busy || empty} onClick={() => void clear(session, false)}>
            {t('previews.settings.discardAllButton')}
          </Button>
        </div>
      </div>
    );
  };

  return (
    <>
      <div className="pref-row">
        <div className="pref-text">
          <div className="pref-label">{t('previews.settings.standardSize')}</div>
          <p className="pref-description">{t('previews.settings.standardSizeDesc')}</p>
        </div>
        <div className="pref-control">
          <Dropdown
            onChange={(value) => void change({ standardPreviewSize: value })}
            options={sizeOptions}
            value={appSettings?.standardPreviewSize ?? 0}
            triggerClassName="bg-bg-primary"
          />
        </div>
      </div>
      <div className="pref-row">
        <div className="pref-text">
          <div className="pref-label">{t('previews.settings.discardFull')}</div>
          <p className="pref-description">{t('previews.settings.discardFullDesc')}</p>
        </div>
        <div className="pref-control">
          <Dropdown
            onChange={(value) => void change({ discardFullPreviewsDays: value })}
            options={discardOptions}
            value={appSettings?.discardFullPreviewsDays ?? 30}
            triggerClassName="bg-bg-primary"
          />
        </div>
      </div>
      <div className="pref-row">
        <div className="pref-text">
          <div className="pref-label">{t('previews.settings.standardLimit')}</div>
          <p className="pref-description">{t('previews.settings.standardLimitDesc')}</p>
        </div>
        <div className="pref-control">
          <Dropdown
            onChange={(value) => void change({ standardPreviewsLimitGb: value })}
            options={limitOptions}
            value={appSettings?.standardPreviewsLimitGb ?? 10}
            triggerClassName="bg-bg-primary"
          />
        </div>
      </div>
      {folderRow(true)}
      {folderRow(false)}
    </>
  );
}
