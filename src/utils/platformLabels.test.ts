import { describe, expect, it } from 'vitest';
import { platformLabel, platformResources, quickSettingsCopy } from './platformLabels';

describe('platform shortcut labels', () => {
  it.each(['windows', 'linux'])('formats legacy translated hints on %s', (platform) => {
    expect(platformLabel('Export (⇧⌘E), Auto (⌘L), Alt/Option-click', platform)).toBe(
      'Export (Ctrl+Shift+E), Auto (Ctrl+L), Alt-click'
    );
    expect(platformLabel('Capture (⌘K / Ctrl+K)', platform)).toBe('Capture (Ctrl+K)');
    expect(platformLabel('Preferences (⌘,)', platform)).toBe('Preferences (Ctrl+,)');
    expect(platformLabel('Reference (⇧Return, ⌘Return)', platform)).toBe('Reference (Shift+Enter, Ctrl+Enter)');
  });
  it('preserves Mac labels', () => {
    const label = 'Export (⇧⌘E), Alt/Option-click';
    expect(platformLabel(label, 'macos')).toBe(label);
  });
});

it('keeps the picker on Mac, in Library, and with Shift; plain copy in Develop is quick', () => {
  for (const platform of ['windows', 'linux']) {
    expect(quickSettingsCopy(platform, 'editor', false)).toBe(true);
    expect(quickSettingsCopy(platform, 'editor', true)).toBe(false);
    expect(quickSettingsCopy(platform, 'library', false)).toBe(false);
  }
  expect(quickSettingsCopy('macos', 'editor', false)).toBe(false);
});

it('formats hard-coded hints in every locale', async () => {
  const locales = import.meta.glob('../i18n/locales/*.json', { eager: true, import: 'default' });
  const visit = (value: unknown) => {
    if (typeof value === 'string') {
      expect(platformLabel(value, 'windows')).not.toMatch(/[⌘⇧⌥⌃]|Alt\/Option/);
      expect(platformLabel(value, 'macos')).toBe(value);
    } else if (value && typeof value === 'object') Object.values(value).forEach(visit);
  };
  expect(Object.keys(locales)).toHaveLength(13);
  Object.values(locales).forEach(visit);
});

it.each(['windows', 'linux', 'macos'])('preserves interpolated names on %s', async (platform) => {
  const { createInstance } = await import('i18next');
  const instance = createInstance();
  const resources = { en: { translation: { hint: '{{name}} (⌘Return), {{tool}}' } } };
  await instance.init({
    resources: platformResources(resources, platform),
    lng: 'en',
    interpolation: { escapeValue: false }
  });
  expect(instance.t('hint', { name: '⌘photo Option', tool: 'My Option' })).toBe(
    platform === 'macos' ? '⌘photo Option (⌘Return), My Option' : '⌘photo Option (Ctrl+Enter), My Option'
  );
  expect(resources.en.translation.hint).toBe('{{name}} (⌘Return), {{tool}}');
});
