// SPDX-License-Identifier: AGPL-3.0-or-later
// Reviewing photographs in the import dialog before they are imported, and
// memory cards as its source (task IMPD; reference: Capture One's importer
// keys and card behaviour, notes reference/ux-import-2026-11-08).
import type { ImportMetadata } from './IngestMetadata';

/** What a key does to the photograph the import grid or large view has. */
export type ReviewKey = { kind: 'include' } | { kind: 'exclude' } | { kind: 'rate'; rating: number };

/** P includes in the import, X leaves out (the Library's pick and reject
 * letters), 0–5 rate it; anything else is not a review key. Modifier keys
 * are left to the app. */
export function reviewKey(e: {
  key: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
}): ReviewKey | null {
  if (e.metaKey || e.ctrlKey || e.altKey) return null;
  const key = e.key.toLowerCase();
  if (key === 'p') return { kind: 'include' };
  if (key === 'x') return { kind: 'exclude' };
  if (/^[0-5]$/.test(key)) return { kind: 'rate', rating: Number(key) };
  return null;
}

/** `selected` and `ratings` after `key` on the photograph at `path`. */
export function applyReviewKey(
  key: ReviewKey,
  path: string,
  selected: Set<string>,
  ratings: Record<string, number>
): { selected: Set<string>; ratings: Record<string, number> } {
  if (key.kind === 'rate') {
    const next = { ...ratings };
    if (key.rating > 0) next[path] = key.rating;
    else delete next[path];
    return { selected, ratings: next };
  }
  const next = new Set(selected);
  if (key.kind === 'include') next.add(path);
  else next.delete(path);
  return { selected: next, ratings };
}

/** The import's metadata with the ratings of the photographs being imported
 * (`files`); the chosen preset as it is when nothing is rated, and null
 * when there is neither. Applied to the copies after their verified copy,
 * never to the card (`ingest::metadata::ImportMetadata::ratings`). */
export function withRatings(
  metadata: ImportMetadata | null,
  ratings: Record<string, number>,
  files: string[]
): ImportMetadata | null {
  const rated = Object.fromEntries(
    files.filter((path) => (ratings[path] ?? 0) > 0).map((path) => [path, ratings[path]])
  );
  if (!Object.keys(rated).length) return metadata;
  return { ...(metadata ?? {}), ratings: rated } as ImportMetadata;
}

/** Mirrors `ingest::cards::Card`. */
export interface MemoryCard {
  mountPoint: string;
  name: string;
}

/** What to do with a memory card just inserted: open the import with it,
 * wait until the running export has finished (no focus steal), or nothing
 * (preference off, an import running, tethering, another dialog open). */
export function cardInsertAction(state: {
  enabled: boolean;
  importing: boolean;
  tethering: boolean;
  exporting: boolean;
  dialogOpen: boolean;
}): 'open' | 'wait' | 'ignore' {
  if (!state.enabled || state.importing || state.tethering || state.dialogOpen) return 'ignore';
  return state.exporting ? 'wait' : 'open';
}

/** Whether `path` is on the card mounted at `mount`. */
export function onCard(path: string, mount: string): boolean {
  const root = mount.replace(/[\\/]+$/, '');
  return path === root || path.startsWith(`${root}/`) || path.startsWith(`${root}\\`);
}

/** Mirrors the parts of `ingest::ImportResult` the eject decision reads. */
interface Result {
  source: string;
  error?: string | null;
}

/** The card to eject after an import of `files` from it: only when every
 * photograph asked for came through without an error (verified copies; a
 * duplicate already in the library counts as safe) and the import was not
 * cut short; null otherwise. */
export function cardToEject(mount: string | null, files: string[], results: Result[]): string | null {
  if (!mount || !files.length || !files.every((file) => onCard(file, mount))) return null;
  const done = new Map(results.map((result) => [result.source, result]));
  return files.every((file) => done.has(file) && !done.get(file)!.error) ? mount : null;
}
