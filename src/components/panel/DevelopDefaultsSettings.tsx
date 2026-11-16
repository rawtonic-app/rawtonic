// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Trash2 } from 'lucide-react';
import Switch from '../ui/Switch';
import Dropdown, { type OptionItem } from '../ui/Dropdown';
import Text from '../ui/Text';
import { TextVariants } from '../../types/typography';
import { type AppSettings, type Preset } from '../ui/AppProperties';
import { loadPresetLibrary, usePresetStore } from '../../hooks/usePresets';
import { loadBuiltinPresets, presetName, useBuiltinPresets } from '../../utils/builtinPresets';
import {
  cameraName,
  NO_DEVELOP_DEFAULTS,
  presetDefaultSettings,
  withGlobalDefault,
  withoutCameraDefault,
  type DevelopDefaults
} from '../../utils/developDefaults';

const NONE = '';

/**
 * Preferences > Develop defaults: the one place for what photographs
 * without an edit start with (G28; user decisions 2026-11-15): the global
 * default preset, the camera models' and bodies' defaults (set from
 * Develop: a preset's or the photo's menu, a tool's menu), RAW files only.
 * Edited photographs never change; never-edited ones follow.
 */
export default function DevelopDefaultsSettings({
  appSettings,
  save
}: {
  appSettings: AppSettings;
  /** Saves new defaults (`saveDevelopDefaults`; tests replace it). */
  save(next: DevelopDefaults): void | Promise<void>;
}) {
  const { t } = useTranslation();
  const defaults = appSettings.developDefaults ?? NO_DEVELOP_DEFAULTS;
  const own = usePresetStore((s) => s.presets);
  const builtins = useBuiltinPresets((s) => s.presets);
  useEffect(() => {
    void loadPresetLibrary();
    void loadBuiltinPresets();
  }, []);
  const presets = useMemo(
    () => [...own.flatMap((item) => (item.preset ? [item.preset] : (item.folder?.children ?? []))), ...builtins],
    [own, builtins]
  );
  const options: OptionItem<string>[] = [
    { value: NONE, label: t('developDefaults.settings.globalNone') },
    ...presets.map((preset: Preset) => ({ value: preset.id, label: presetName(t, preset) }))
  ];
  const chosen = defaults.global
    ? (presets.find((preset) => presetName(t, preset) === defaults.global?.name)?.id ?? null)
    : NONE;
  const chooseGlobal = (id: string) => {
    const preset = presets.find((p) => p.id === id);
    void save(
      withGlobalDefault(
        defaults,
        preset ? { name: presetName(t, preset), settings: presetDefaultSettings(preset) } : null
      )
    );
  };
  const cameras = defaults.cameras ?? [];
  return (
    <div className="space-y-4" data-testid="develop-defaults">
      <p className="pref-description">{t('developDefaults.settings.intro')}</p>
      <div className="pref-row">
        <div className="pref-text">
          <div className="pref-label">{t('developDefaults.settings.global')}</div>
          <p className="pref-description">{t('developDefaults.settings.globalDesc')}</p>
          {defaults.global && chosen === null && (
            <Text variant={TextVariants.small} color="secondary">
              {t('developDefaults.settings.globalFrom', { name: defaults.global.name ?? '' })}
            </Text>
          )}
        </div>
        <div className="pref-control">
          <Dropdown
            value={chosen}
            options={options}
            placeholder={defaults.global?.name ?? t('developDefaults.settings.globalNone')}
            onChange={chooseGlobal}
            triggerClassName="bg-bg-primary"
          />
        </div>
      </div>
      <div className="pref-row">
        <div className="pref-text">
          <div className="pref-label">{t('developDefaults.settings.rawOnly')}</div>
          <p className="pref-description">{t('developDefaults.settings.rawOnlyDesc')}</p>
        </div>
        <div className="pref-control">
          <Switch
            checked={defaults.rawOnly !== false}
            id="develop-defaults-raw-only"
            label={t('developDefaults.settings.rawOnly')}
            onChange={(checked) => void save({ ...defaults, rawOnly: checked })}
          />
        </div>
      </div>
      <div>
        <div className="pref-label">{t('developDefaults.settings.cameras')}</div>
        <p className="pref-description">{t('developDefaults.settings.camerasDesc')}</p>
        {cameras.length === 0 ? (
          <Text variant={TextVariants.small} color="secondary">
            {t('developDefaults.settings.camerasNone')}
          </Text>
        ) : (
          <ul className="mt-2 space-y-1" aria-label={t('developDefaults.settings.cameras')}>
            {cameras.map((entry, index) => (
              <li
                key={`${entry.make}|${entry.model}|${entry.serial ?? ''}`}
                className="flex items-center justify-between gap-2 rounded border border-border-color px-2 py-1"
              >
                <div className="min-w-0">
                  <Text>
                    {entry.serial
                      ? t('developDefaults.bodyName', { camera: cameraName(entry), serial: entry.serial })
                      : cameraName(entry)}
                  </Text>
                  <Text variant={TextVariants.small} color="secondary">
                    {[
                      entry.name,
                      entry.inherit ? t('developDefaults.settings.joins') : t('developDefaults.settings.replaces')
                    ]
                      .filter(Boolean)
                      .join(' · ')}
                  </Text>
                </div>
                <button
                  type="button"
                  aria-label={t('developDefaults.settings.remove', {
                    camera: cameraName(entry)
                  })}
                  title={t('developDefaults.settings.remove', { camera: cameraName(entry) })}
                  onClick={() => void save(withoutCameraDefault(defaults, index))}
                >
                  <Trash2 size={14} />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
