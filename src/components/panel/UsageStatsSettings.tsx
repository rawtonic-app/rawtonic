// SPDX-License-Identifier: AGPL-3.0-or-later
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import Switch from '../ui/Switch';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';
import { type AppSettings } from '../ui/AppProperties';
import {
  plainLines,
  setUsageStatsEnabled,
  usageStatsPreview,
  type UsageStatsPreview,
} from '../contribute/usageStats';

/** Anonymous weekly usage statistics (ADR 0017, channel 4): off unless
 * switched on here (the app saves that choice itself, `usage_stats_set_enabled`). Switching on shows what is sent before anything is;
 * "Show what is sent" shows it at any time, in plain words or as the exact
 * JSON body; switching off deletes what was kept, so nothing more is sent. */
export default function UsageStatsSettings({
  appSettings,
  onSettingsChange,
  preview = usageStatsPreview,
  enable = setUsageStatsEnabled,
}: {
  appSettings: AppSettings;
  onSettingsChange(settings: AppSettings): void | Promise<void>;
  /** Tests replace the app's call. */
  preview?(): Promise<UsageStatsPreview>;
  enable?(on: boolean): Promise<void>;
}) {
  const { t } = useTranslation();
  const on = appSettings.usageStats === true;
  const [showing, setShowing] = useState(false);
  const [raw, setRaw] = useState(false);
  const [shown, setShown] = useState<UsageStatsPreview | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    preview()
      .then(setShown)
      .catch(() => setShown({}));
  }, [preview]);

  useEffect(() => {
    if (showing) load();
  }, [load, showing, on]);

  const toolName = (tool: string) => t(`contribute.stats.tools.${tool}`, { defaultValue: tool });
  const body = (text: string) =>
    raw ? (
      <pre className="mt-1 max-h-60 overflow-auto rounded bg-bg-primary p-2 font-mono text-xs whitespace-pre-wrap wrap-anywhere">
        {text}
      </pre>
    ) : (
      <ul className="mt-1 flex flex-col gap-1 text-sm">
        {plainLines(text, t, toolName).map((line) => (
          <li key={line}>{line}</li>
        ))}
      </ul>
    );

  return (
    <div className="flex flex-col gap-2">
      <div className="pref-row">
        <div className="pref-text">
          <div className="pref-label">{t('contribute.stats.setting')}</div>
          <p className="pref-description">{t('contribute.stats.description')}</p>
          {error && (
            <Text variant={TextVariants.small} color="error" role="alert">
              {error}
            </Text>
          )}
          <button type="button" className="mt-1 text-[11px] underline" onClick={() => setShowing((v) => !v)}>
            {showing ? t('contribute.stats.hide') : t('contribute.stats.show')}
          </button>
        </div>
        <div className="pref-control">
          <Switch
            checked={on}
            label={t('contribute.stats.setting')}
            onChange={(checked) => {
              void (async () => {
                setError(null);
                // The app saves the choice itself and says when it could not:
                // a choice shown here is a choice kept.
                try {
                  await enable(checked);
                } catch {
                  setError(checked ? t('contribute.stats.onNotSaved') : t('contribute.stats.offNotSaved'));
                  load();
                  return;
                }
                await onSettingsChange({ ...appSettings, usageStats: checked });
                // Before the first send: what will be sent.
                if (checked) setShowing(true);
                else load();
              })();
            }}
          />
        </div>
      </div>
      {showing && shown && (
        <div className="rounded border border-surface p-3 flex flex-col gap-3" data-testid="usage-stats-preview">
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" checked={raw} onChange={(e) => setRaw(e.target.checked)} />
            {t('contribute.stats.raw')}
          </label>
          {on && shown.pending?.length ? (
            shown.pending.map((item) => (
              <div key={item.week}>
                <Text>{item.current ? t('contribute.stats.current') : t('contribute.stats.due')}</Text>
                {body(item.body)}
              </div>
            ))
          ) : (
            <Text variant={TextVariants.small} color="secondary">
              {on ? t('contribute.stats.nothingYet') : t('contribute.stats.off')}
            </Text>
          )}
          {shown.lastSent && (
            <div>
              <Text>{t('contribute.stats.lastSent', { week: shown.lastSent.week })}</Text>
              {body(shown.lastSent.body)}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
