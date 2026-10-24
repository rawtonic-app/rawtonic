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
  if (isUnreadableError(text)) {
    return i18next.t('studio.loadError.unreadable', { name });
  }
  return i18next.t('studio.loadError.failed', { name, error: text.replace(/^Error:\s*/, '') });
}

/** Whether a load error is the decoder's: a file it cannot read (rawler's
 * "Unknown camera", "No decoder found", …), not a missing file or a failed
 * read. */
export function isUnreadableError(error: unknown): boolean {
  return /no decoder found|unknown camera|unsupported|not a valid|invalid (raw|tiff|image)|decod/i.test(String(error));
}
