/**
 * The identity of a photo in a library: its source file, and for a virtual
 * copy the copy's id. Twin of `crates/metadata/src/photo_ref.rs`; both read
 * the same test vectors (`photo_ref_vectors.json` there).
 *
 * Stored and sent as one string: the source path for the primary photo,
 * `<source>?vc=<copy id>` for a virtual copy. `formatPhotoRef(parsePhotoRef(text))`
 * gives `text` back for every string. The copy id is what follows the *last*
 * `?vc=`, so a copy of a file whose name contains `?vc=` keeps its source.
 */

export const COPY_MARKER = '?vc=';

export interface PhotoRef {
  /** The source file's path as stored. */
  source: string;
  /** The virtual copy's id; `null` for the primary photo. */
  copy: string | null;
}

export function parsePhotoRef(text: string): PhotoRef {
  const at = text.lastIndexOf(COPY_MARKER);
  return at < 0
    ? { source: text, copy: null }
    : { source: text.slice(0, at), copy: text.slice(at + COPY_MARKER.length) };
}

export function formatPhotoRef({ source, copy }: PhotoRef): string {
  return copy === null ? source : `${source}${COPY_MARKER}${copy}`;
}

/** The source file's path of a stored path (a virtual copy's `?vc=` part removed). */
export function sourceOf(text: string): string {
  return parsePhotoRef(text).source;
}

/** The copy id of a stored path; `null` for a primary photo. */
export function copyOf(text: string): string | null {
  return parsePhotoRef(text).copy;
}

/** Whether a stored path names a virtual copy. */
export function isCopy(text: string): boolean {
  return text.includes(COPY_MARKER);
}

/** Whether the stored path `text` names a virtual copy of `source`. */
export function isCopyOf(text: string, source: string): boolean {
  const ref = parsePhotoRef(text);
  return ref.copy !== null && ref.source === source;
}

/** The same photo (same copy) of another source file, e.g. after a move. */
export function withSource(text: string, source: string): string {
  return formatPhotoRef({ source, copy: copyOf(text) });
}

/** The source file's name (last path part, `/` or `\` separated). */
export function sourceFileName(text: string): string {
  return sourceOf(text).split(/[\\/]/).pop() ?? '';
}
