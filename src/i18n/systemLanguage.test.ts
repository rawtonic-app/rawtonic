import { afterEach, expect, it, vi } from 'vitest';
import { locale } from '@tauri-apps/plugin-os';
import { interfaceLanguage, matchUiLanguage, systemLanguage } from './systemLanguage';

vi.mock('@tauri-apps/plugin-os', () => ({ locale: vi.fn() }));

afterEach(() => vi.restoreAllMocks());

it('maps system language tags to a shipped interface language (UXR4L-03, UXR4M-04)', () => {
  expect(matchUiLanguage('de-CH')).toBe('de');
  expect(matchUiLanguage('de_CH.UTF-8')).toBe('de');
  expect(matchUiLanguage('fr_FR.UTF-8@euro')).toBe('fr');
  expect(matchUiLanguage('pt-BR')).toBe('pt');
  expect(matchUiLanguage('ca-ES-valencia')).toBe('ca');
  expect(matchUiLanguage('zh-Hant-TW')).toBe('zh-TW');
  expect(matchUiLanguage('zh_HK')).toBe('zh-TW');
  expect(matchUiLanguage('zh-Hans-CN')).toBe('zh-CN');
  expect(matchUiLanguage('zh-SG')).toBe('zh-CN');
  expect(matchUiLanguage('EN-us')).toBe('en');
  expect(matchUiLanguage('nl-NL')).toBeNull();
  expect(matchUiLanguage('C')).toBeNull();
  expect(matchUiLanguage('POSIX')).toBeNull();
  expect(matchUiLanguage('')).toBeNull();
});

it("follows the system's language, else the webview's, else English", async () => {
  vi.mocked(locale).mockResolvedValue('de-CH');
  expect(await systemLanguage()).toBe('de');
  vi.mocked(locale).mockResolvedValue('nl-NL');
  vi.spyOn(navigator, 'languages', 'get').mockReturnValue(['nl-NL', 'fr-BE']);
  expect(await systemLanguage()).toBe('fr');
  vi.mocked(locale).mockRejectedValue(new Error('not in the app'));
  vi.spyOn(navigator, 'languages', 'get').mockReturnValue(['nl-NL']);
  expect(await systemLanguage()).toBe('en');
});

it('keeps a language chosen in Preferences over the system language', async () => {
  vi.mocked(locale).mockResolvedValue('ja-JP');
  expect(await interfaceLanguage('de')).toBe('de');
  expect(await interfaceLanguage(undefined)).toBe('ja');
  expect(await interfaceLanguage(null)).toBe('ja');
  // A language Rawtonic no longer ships counts as no choice.
  expect(await interfaceLanguage('xx')).toBe('ja');
});
