import { locale } from '@tauri-apps/plugin-os';

/** The interface languages Rawtonic ships (src/i18n/locales). */
export const UI_LANGUAGES = ['en', 'ca', 'de', 'es', 'fr', 'it', 'pl', 'pt', 'ru', 'ja', 'ko', 'zh-CN', 'zh-TW'] as const;

/**
 * The shipped interface language for a system language tag — a BCP 47 tag
 * (`de-CH`, `zh-Hant-TW`) or a POSIX locale (`de_CH.UTF-8@euro`) — or null
 * when none fits. A regional variant takes its language (`de-CH` → `de`,
 * `pt-BR` → `pt`); Chinese picks the script: Traditional for `Hant`, Taiwan,
 * Hong Kong and Macau, else Simplified.
 */
export function matchUiLanguage(tag: string | null | undefined): string | null {
  if (!tag) return null;
  const parts = tag
    .replace(/[.@].*$/, '')
    .replace(/_/g, '-')
    .toLowerCase()
    .split('-')
    .filter(Boolean);
  const [language, ...rest] = parts;
  if (!language || language === 'c' || language === 'posix') return null;
  if (language === 'zh') {
    return rest.some((part) => ['hant', 'tw', 'hk', 'mo'].includes(part)) ? 'zh-TW' : 'zh-CN';
  }
  return (UI_LANGUAGES as readonly string[]).includes(language) ? language : null;
}

/**
 * The language a first start shows (UXR4L-03, UXR4M-04): the system's
 * interface language when Rawtonic ships it, else English. The system's
 * own setting first (Windows' display language, macOS' preferred
 * languages, `LANGUAGE`/`LC_ALL`/`LC_MESSAGES`/`LANG` on Linux), then the
 * webview's languages.
 */
export async function systemLanguage(): Promise<string> {
  let system: string | null = null;
  try {
    system = await locale();
  } catch {
    // Outside the native app (tests, `npm run dev`): the webview's languages.
  }
  const candidates = [system, ...(typeof navigator === 'undefined' ? [] : (navigator.languages ?? [navigator.language]))];
  for (const candidate of candidates) {
    const match = matchUiLanguage(candidate);
    if (match) return match;
  }
  return 'en';
}

/** The interface language: the one chosen in Preferences, else the system's. */
export async function interfaceLanguage(chosen: string | null | undefined): Promise<string> {
  const explicit = chosen && (UI_LANGUAGES as readonly string[]).includes(chosen) ? chosen : null;
  return explicit ?? (await systemLanguage());
}
