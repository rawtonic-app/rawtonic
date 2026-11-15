// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * Library-wide keyword changes from the Keyword List (G36/G37): rename,
 * merge, move under another keyword and convert a legacy flat `a > b`
 * keyword into its hierarchy are one operation, a rename of a path with
 * every keyword under it (`rename_keyword_for_paths` in
 * src-tauri/src/tagging.rs). It writes each photograph's sidecar (and XMP
 * when syncing) through the photographs' write queue and is journaled as a
 * batch, so a quit half way is offered for completion (G66).
 */
import { useEffect, useSyncExternalStore } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { saveFailure, sortStoredTags } from '../hooks/useLibraryActions';
import { useLibraryStore } from '../store/useLibraryStore';
import { invokeInOrder, onCatalogChange } from './catalogWrites';
import { searchLibrary, type FacetValue, type Facets } from './librarySearch';
import { catalogChanged } from './catalogEvents';
import {
  keywordAction,
  savedKeywordChanges,
  type DeclaredChange,
  type KeywordChange,
  type KeywordRecorder
} from './appUndo';
import {
  countKeywordPaths,
  deleteKeywordTags,
  renameKeywordTags,
  tagsHaveKeyword,
  withDeclaredKeywords
} from './keywordTree';

/** Every keyword of the library (or, outside one, of the listed
 * photographs) with its photographs, each parent counted too. */
export async function libraryKeywords(directory: string | null): Promise<FacetValue[]> {
  if (!directory) {
    const keywords = useLibraryStore
      .getState()
      .imageList.map((image) =>
        (image.tags ?? []).filter((tag) => !tag.startsWith('color:')).map((tag) => tag.replace(/^user:/, ''))
      );
    return countKeywordPaths(keywords);
  }
  const facets = await invoke<Facets>(Invokes.SessionFacets, {
    directory,
    requests: [{ field: 'keyword', rule: { combine: 'all', rules: [] } }],
    listed: null
  });
  const counted = (facets?.keyword ?? []).filter((value) => value.value !== '');
  // Keywords made without photographs are listed with a count of 0.
  const declared = await invoke<string[]>(Invokes.SessionKeywordsDeclared, { directory }).catch(() => []);
  return Array.isArray(declared) && declared.length ? withDeclaredKeywords(counted, declared) : counted;
}

/** The library's keywords, shared by the Keywords and Keyword List tools:
 * one count per change, however many tools ask. */
let shared: FacetValue[] = [];
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setTimeout> | undefined;
let asked = 0;
function refresh(directory: string | null) {
  clearTimeout(timer);
  timer = setTimeout(() => {
    const mine = ++asked;
    libraryKeywords(directory)
      .then((values) => {
        if (mine !== asked) return;
        shared = values;
        for (const listener of listeners) listener();
      })
      .catch((e) => console.error('Keyword list failed:', e));
  }, 0);
}
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

/** The library's keywords as last counted for a tool that keeps them
 * (`useLibraryKeywords`), without asking the catalog. */
export const useKnownKeywords = (): FacetValue[] => useSyncExternalStore(subscribe, () => shared);

/** Every keyword of the library with its photographs (`libraryKeywords`),
 * kept current over catalog changes and new listings. */
export function useLibraryKeywords(): FacetValue[] {
  const directory = useLibraryStore((s) => s.sessionDirectory);
  const imageList = useLibraryStore((s) => s.imageList);
  useEffect(() => {
    refresh(directory);
    return onCatalogChange(() => refresh(directory));
  }, [directory, imageList]);
  return useSyncExternalStore(subscribe, () => shared);
}

/** The photographs holding `path` or a keyword under it: the library's
 * (by its catalog), its virtual copies with keywords of their own (which
 * the keyword index does not hold, wherever they are listed) and the
 * listed ones. */
export async function photosWithKeyword(directory: string | null, path: string): Promise<string[]> {
  const [found, copies] = directory
    ? await Promise.all([
        searchLibrary(directory, { field: 'keyword', op: 'is', value: path }),
        invoke<string[]>(Invokes.SessionKeywordCopies, { directory, path })
      ])
    : [[], []];
  const paths = new Set([...found, ...(copies ?? [])]);
  for (const image of useLibraryStore.getState().imageList)
    if (tagsHaveKeyword(image.tags, path)) paths.add(image.path);
  return [...paths];
}

/** Sets the listed photographs' tags as a write left them: `change` is
 * applied to the tags of each of `written`. */
function showWritten(written: string[], change: (tags: string[]) => string[]) {
  const targets = new Set(written);
  useLibraryStore.getState().setLibrary((state) => ({
    imageList: state.imageList.map((image) =>
      targets.has(image.path) ? { ...image, tags: sortStoredTags(change(image.tags ?? [])) } : image
    )
  }));
}

/** Runs one keyword write on `paths`: reports what saved to `record` (also
 * for a write that failed for some photographs, which keep their keywords),
 * shows it on the listed photographs and rejects with the backend's
 * `SaveFailure` when some were not written. */
export async function writeKeywords(
  paths: string[],
  record: KeywordRecorder,
  change: (tags: string[]) => string[],
  write: () => Promise<KeywordChange[]>
) {
  if (!paths.length) return;
  try {
    record({ changes: await write() });
    showWritten(paths, change);
  } catch (e) {
    const { failed } = saveFailure(e, paths);
    record({ changes: savedKeywordChanges(e) });
    showWritten(
      paths.filter((path) => !failed.includes(path)),
      change
    );
    throw e;
  }
}

/** Renames `from` (with the keywords under it) to `to` on `paths`, and the
 * listed photographs' tags as written. Rejects with the backend's
 * `SaveFailure` when some photographs were not written (they keep theirs). */
export const renameKeywordOn = (paths: string[], from: string, to: string, record: KeywordRecorder) =>
  writeKeywords(
    paths,
    record,
    (tags) => renameKeywordTags(tags, from, to),
    () => invokeInOrder<KeywordChange[]>(Invokes.RenameKeywordForPaths, { paths, from, to }, 'keyword')
  );

/** Deletes the keyword `path` from `paths`: its children move up a level. */
export const deleteKeywordOn = (paths: string[], path: string, record: KeywordRecorder) =>
  writeKeywords(
    paths,
    record,
    (tags) => deleteKeywordTags(tags, path),
    () => invokeInOrder<KeywordChange[]>(Invokes.DeleteKeywordForPaths, { paths, path }, 'keyword')
  );

/** The keywords made without photographs follow a rename or delete of the
 * keyword they are within. */
export async function declaredKeywordsFollow(
  directory: string | null,
  command: 'rename' | 'delete',
  path: string,
  to?: string
): Promise<DeclaredChange | undefined> {
  if (!directory) return undefined;
  const change =
    command === 'rename'
      ? await invoke<DeclaredChange>(Invokes.SessionKeywordsDeclaredRename, { directory, from: path, to })
      : await invoke<DeclaredChange>(Invokes.SessionKeywordsDeclaredDelete, { directory, path });
  catalogChanged();
  return change;
}

/** Adds the stored keyword `tag` to `paths` as one undoable step named for
 * `keyword` (Undo Add Keyword 'Dogs'); the photographs that have it already
 * are left out. Rejects with the `SaveFailure` of a partly saved write. */
export function addKeywordTo(paths: string[], tag: string, keyword: string) {
  const tagsOf = new Map(useLibraryStore.getState().imageList.map((image) => [image.path, image.tags]));
  const missing = paths.filter((path) => !(tagsOf.get(path) ?? []).includes(tag));
  if (!missing.length) return Promise.resolve();
  return keywordAction(
    { kind: 'add', keyword },
    (record) =>
      writeKeywords(
        paths,
        record,
        (tags) => (tags.includes(tag) ? tags : [...tags, tag]),
        () => invokeInOrder<KeywordChange[]>(Invokes.AddTagForPaths, { paths, tag }, 'keyword')
      ),
    paths
  );
}

/** Makes the keyword `path` without photographs (New Keyword) as one
 * undoable step; true when the library had no such keyword yet. */
export async function makeKeyword(directory: string, path: string): Promise<boolean> {
  let added = false;
  await keywordAction({ kind: 'new', keyword: path }, async (record) => {
    const declared = await invoke<DeclaredChange>(Invokes.SessionKeywordDeclare, { directory, path });
    record({ declared });
    added = declared.added.length > 0;
    catalogChanged();
  });
  return added;
}

/** Deletes the keywords without photographs in `paths` (Delete Unused
 * Keywords) as one undoable step. */
export function deleteUnusedKeywords(directory: string, paths: string[]) {
  return keywordAction({ kind: 'deleteUnused' }, async (record) => {
    const declared = await invoke<DeclaredChange>(Invokes.SessionKeywordsDeclaredApply, {
      directory,
      remove: paths,
      add: []
    });
    record({ declared });
    catalogChanged();
  });
}
