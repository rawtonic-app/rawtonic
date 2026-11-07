// SPDX-License-Identifier: AGPL-3.0-or-later
// Content search (G37, `content_search.rs`): the search box's Content mode
// sorts the current view by how well each photograph matches the text. These
// are the frontend's calls and the per-library mode; the payloads are pinned
// in src/contracts/contentSearch.json, which Rust deserializes too.
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { noteTool } from '../components/contribute/usageStats';

/** What the search box searches: words in names, keywords and metadata, or
 * what the photographs show. */
export type SearchMode = 'text' | 'content';

/** How far the library's index is (`ContentStatus`). */
export interface ContentIndex {
  indexed: number;
  failed: number;
  total: number;
}

/** `content_search_status`: the model and the index. */
export interface ContentSearchStatus extends ContentIndex {
  installed: boolean;
  /** Bytes the download takes (0 when installed). */
  downloadBytes: number;
  running: boolean;
}

/** `content_search_rank`: every indexed photograph, best match first. */
export interface ContentRankingAnswer extends ContentIndex {
  paths: string[];
}

/** `content-index-progress` and `content-index-done` (`cancelled` only there). */
export interface ContentIndexProgress extends ContentIndex {
  directory: string;
  cancelled?: boolean;
}
export const CONTENT_INDEX_PROGRESS = 'content-index-progress';
export const CONTENT_INDEX_DONE = 'content-index-done';

const modeKey = (library: string) => `rawtonic.search.mode:${library}`;

/** The mode last used in `library` (Text when none). */
export function readSearchMode(library: string | null): SearchMode {
  if (!library) return 'text';
  try {
    return localStorage.getItem(modeKey(library)) === 'content' ? 'content' : 'text';
  } catch {
    return 'text';
  }
}

export function writeSearchMode(library: string | null, mode: SearchMode): void {
  if (!library) return;
  try {
    localStorage.setItem(modeKey(library), mode);
  } catch {
    // Kept for this session only.
  }
}

export const startContentIndex = (directory: string, resume: boolean) =>
  invoke<void>(Invokes.StartContentIndex, { directory, resume });

export const stopContentIndex = (directory: string | null) => invoke<void>(Invokes.StopContentIndex, { directory });

export const contentSearchStatus = (directory: string) =>
  invoke<ContentSearchStatus>(Invokes.ContentSearchStatus, { directory });

export const downloadContentSearch = (directory: string) => invoke<void>(Invokes.ContentSearchDownload, { directory });

/** `listed`: the folder the grid lists, perhaps through a symlink; the
 * answer comes in its form. */
export const rankByContent = (directory: string, query: string, listed: string | null) => {
  noteTool('search');
  return invoke<ContentRankingAnswer>(Invokes.ContentSearchRank, { directory, query, listed });
};

/** A download size as the button shows it ("1.5 GB"). */
export function downloadSize(bytes: number, locale?: string): string {
  const gb = bytes / 1e9;
  if (gb >= 0.1) return `${gb.toLocaleString(locale, { minimumFractionDigits: 1, maximumFractionDigits: 1 })} GB`;
  return `${Math.max(1, Math.round(bytes / 1e6)).toLocaleString(locale)} MB`;
}

/**
 * `list` sorted by `ranks` (photograph path → place, best 0): ranked
 * photographs first by place (a virtual copy with its photograph), then the
 * others in their order. Stable, so equal places keep the list's order.
 */
export function sortByMatch<T>(list: T[], ranks: Map<string, number>, photograph: (item: T) => string): T[] {
  const keyed = list.map((item, index) => ({ item, index, rank: ranks.get(photograph(item)) }));
  keyed.sort((a, b) => {
    if (a.rank !== undefined && b.rank !== undefined) return a.rank - b.rank || a.index - b.index;
    if (a.rank !== undefined) return -1;
    if (b.rank !== undefined) return 1;
    return a.index - b.index;
  });
  return keyed.map(({ item }) => item);
}
