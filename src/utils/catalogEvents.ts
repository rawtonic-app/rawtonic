/** Fired (on `window`) after a write to the library's catalog settled, so
 * searches, value lists and smart albums show what it holds now. */
export const CATALOG_CHANGED = 'rawtonic:catalog-changed';

export function catalogChanged() {
  // A write may settle after the page (or a test's document) is gone.
  if (typeof window !== 'undefined') window.dispatchEvent(new Event(CATALOG_CHANGED));
}

/** A journaled file operation (including trash/restore/undo) settled. */
export const FILE_OPERATIONS_CHANGED = 'rawtonic:file-operations-changed';
