import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useLibraryStore } from '../store/useLibraryStore';
import { CATALOG_CHANGED, catalogChanged } from '../utils/catalogWrites';
import { photographPath, searchLibrary, searchRule } from '../utils/librarySearch';

/** How long typing pauses before the catalog is asked. */
export const SEARCH_DELAY_MS = 80;
/** The background pass recorded capture facts (`library_search::FACTS_RECORDED`). */
export const FACTS_RECORDED = 'library-facts-recorded';

/**
 * Keeps `catalogMatches` to what the library's catalog finds for the search
 * box and the chosen metadata values: asked when either changes (after a
 * short pause in typing) and again after every catalog write, so the grid
 * follows keyword, rating or label changes. Outside a library, or with
 * nothing to search, it is null and the grid filters by itself.
 */
export function useCatalogSearch() {
  const directory = useLibraryStore((s) => s.sessionDirectory);
  const text = useLibraryStore((s) => s.searchCriteria.text);
  const selection = useLibraryStore((s) => s.filterCriteria.metadata);

  // Facts the background pass recorded (a library opened after an update,
  // photographs added by an edit) are catalog changes too: searches, lists
  // and smart albums follow them.
  useEffect(() => {
    if (!directory) return;
    let stop: (() => void) | undefined;
    let gone = false;
    listen(FACTS_RECORDED, () => catalogChanged())
      .then((unlisten) => (gone ? unlisten() : (stop = unlisten)))
      .catch(() => {});
    return () => {
      gone = true;
      stop?.();
    };
  }, [directory]);

  useEffect(() => {
    const rule = directory ? searchRule(text, selection) : null;
    const { setLibrary } = useLibraryStore.getState();
    if (!directory || !rule) {
      setLibrary({ catalogMatches: null });
      return;
    }
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const ask = (delay: number) => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        searchLibrary(directory, rule)
          .then((paths) => {
            // A newer search, or another library, has taken over.
            if (cancelled || useLibraryStore.getState().sessionDirectory !== directory) return;
            setLibrary({ catalogMatches: new Set(paths.map(photographPath)) });
          })
          .catch((e) => console.error('Library search failed:', e));
      }, delay);
    };
    ask(SEARCH_DELAY_MS);
    const changed = () => ask(SEARCH_DELAY_MS);
    window.addEventListener(CATALOG_CHANGED, changed);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      window.removeEventListener(CATALOG_CHANGED, changed);
    };
  }, [directory, text, selection]);
}
