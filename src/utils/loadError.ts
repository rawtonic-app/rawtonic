import i18next from 'i18next';

/**
 * What a failed photo load tells the photographer. The decoder's own words
 * ("No decoder found, model '', make: ''") mean nothing to them: a file it
 * cannot read is damaged, not the RAW file its name says, or from a camera
 * not supported yet (as Lightroom's "unsupported or damaged").
 */
export function loadErrorText(error: unknown, path: string): string {
  const name = path.split(/[\\/]/).pop() ?? path;
  const text = String(error);
  if (/no decoder found|unsupported|not a valid|invalid (raw|tiff|image)|decod/i.test(text)) {
    return i18next.t('studio.loadError.unreadable', { name });
  }
  return i18next.t('studio.loadError.failed', { name, error: text.replace(/^Error:\s*/, '') });
}
