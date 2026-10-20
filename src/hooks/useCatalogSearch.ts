import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useLibraryStore } from '../store/useLibraryStore';
import { catalogChanged, onCatalogChange } from '../utils/catalogWrites';
import { photographPath, searchLibrary, searchRule } from '../utils/librarySearch';

/** How long typing pauses before the catalog is asked. */
export const SEARCH_DELAY_MS = 80;
/** The background pass recorded capture facts (`library_search::FACTS_RECORDED`). */
export const FACTS_RECORDED = 'library-facts-recorded';

/**
 * Keeps `catalogMatches` to what the library's catalog finds for the search
 * box and the chosen metadata values: asked when either changes (after a
 * short pause in typing) and again after every catalog change (writes,
 * imports, relinks: `onCatalogChange`), so the grid follows keyword,
 * rating or label changes and newly imported photographs. Outside a library, or with
 * nothing to search, it is null and the grid filters by itself.
 */
export function useCatalogSearch() {
  const directory = useLibraryStore((s) => s.sessionDirectory);
  const text = useLibraryStore((s) => s.searchCriteria.text);
  const selection = useLibraryStore((s) => s.filterCriteria.metadata);
  // A new list (a folder refreshed after a rename, move or background
  // tagging) is asked about again: its paths or keywords may have changed.
  const imageList = useLibraryStore((s) => s.imageList);

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
    // Only the newest request publishes: an older one answering late (a
    // search asked before a write) must not replace a newer answer.
    let generation = 0;
    const ask = (delay: number) => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        const mine = ++generation;
        searchLibrary(directory, rule)
          .then((paths) => {
            // A newer search, or another library, has taken over.
            if (cancelled || mine !== generation || useLibraryStore.getState().sessionDirectory !== directory) return;
            setLibrary({ catalogMatches: new Set(paths.map(photographPath)) });
          })
          .catch((e) => console.error('Library search failed:', e));
      }, delay);
    };
    ask(SEARCH_DELAY_MS);
    const stop = onCatalogChange(() => ask(SEARCH_DELAY_MS));
    return () => {
      cancelled = true;
      clearTimeout(timer);
      stop();
    };
  }, [directory, text, selection, imageList]);
}
