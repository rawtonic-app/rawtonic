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
import { countKeywordPaths, renameKeywordTags, tagsHaveKeyword } from './keywordTree';

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
  return (facets?.keyword ?? []).filter((value) => value.value !== '');
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

/** Renames `from` (with the keywords under it) to `to` on `paths`, and the
 * listed photographs' tags as written. Rejects with the backend's
 * `SaveFailure` when some photographs were not written (they keep theirs). */
export async function renameKeywordOn(paths: string[], from: string, to: string) {
  if (!paths.length) return;
  const apply = (written: string[]) => {
    const targets = new Set(written);
    useLibraryStore.getState().setLibrary((state) => ({
      imageList: state.imageList.map((image) =>
        targets.has(image.path)
          ? { ...image, tags: sortStoredTags(renameKeywordTags(image.tags ?? [], from, to)) }
          : image
      )
    }));
  };
  try {
    await invokeInOrder(Invokes.RenameKeywordForPaths, { paths, from, to }, 'keyword');
    apply(paths);
  } catch (e) {
    const { failed } = saveFailure(e, paths);
    apply(paths.filter((path) => !failed.includes(path)));
    throw e;
  }
}
