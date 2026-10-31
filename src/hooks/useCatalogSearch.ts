import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import i18n from 'i18next';
import { notify } from '../activity/notify';
import { useLibraryStore } from '../store/useLibraryStore';
import { catalogChanged, onCatalogChange } from '../utils/catalogWrites';
import { useSettingsStore } from '../store/useSettingsStore';
import { folderViewMode } from '../utils/subfolderView';
import {
  photographPath,
  scopeRule,
  searchLibrary,
  searchRule,
  type Rule,
} from '../utils/librarySearch';

/** How long typing pauses before the catalog is asked. */
export const SEARCH_DELAY_MS = 80;
/** The background pass recorded capture facts (`library_search::FACTS_RECORDED`). */
export const FACTS_RECORDED = 'library-facts-recorded';
/** An edit changed what a recipe says about AI (`library_search::AI_FACTS_CHANGED`). */
export const AI_FACTS_CHANGED = 'library-ai-facts-changed';
/** Eyes were read or called (`face_eyes::EYES_CHANGED`). */
export const EYES_CHANGED = 'library-eyes-changed';

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
  const eyes = useLibraryStore((s) => s.filterCriteria.eyes);
  // A new list (a folder refreshed after a rename, move or background
  // tagging) is asked about again: its paths or keywords may have changed.
  const imageList = useLibraryStore((s) => s.imageList);

  // Facts the background pass recorded (a library opened after an update,
  // photographs added by an edit), edits that changed a recipe's AI facts
  // and eyes read or called are catalog changes too: searches, lists and smart albums follow
  // them.
  useEffect(() => {
    if (!directory) return;
    const stops: (() => void)[] = [];
    let gone = false;
    for (const event of [FACTS_RECORDED, AI_FACTS_CHANGED, EYES_CHANGED])
      listen(event, () => catalogChanged())
        .then((unlisten) => (gone ? unlisten() : stops.push(unlisten)))
        .catch(() => {});
    return () => {
      gone = true;
      for (const stop of stops) stop();
    };
  }, [directory]);

  useEffect(() => {
    const rule = directory ? searchRule(text, selection, eyes) : null;
    const { setLibrary } = useLibraryStore.getState();
    if (!directory || !rule) {
      setLibrary({ catalogMatches: null, catalogKnown: null });
      return;
    }
    // Which shown photographs the catalog knows (a listing also shows files
    // it does not hold, e.g. a session's Output; those are matched here).
    const { currentFolderPath, activeAlbumId, subfolderDefaultRoot } = useLibraryStore.getState();
    const viewMode = currentFolderPath
      ? folderViewMode(currentFolderPath, useSettingsStore.getState().appSettings, subfolderDefaultRoot)
      : undefined;
    const scope = scopeRule(currentFolderPath, activeAlbumId, viewMode);
    // A folder the grid lists (not an album view): answers come in its form.
    const listed = activeAlbumId ? null : currentFolderPath;
    const search = (asked: Rule) => searchLibrary(directory, asked, listed);
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // Only the newest request publishes: an older one answering late (a
    // search asked before a write) must not replace a newer answer.
    let generation = 0;
    const ask = (delay: number) => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        const mine = ++generation;
        const current = () =>
          !cancelled && mine === generation && useLibraryStore.getState().sessionDirectory === directory;
        const paths = (asked: Promise<string[]>) => asked.then((list) => new Set(list.map(photographPath)));
        Promise.all([
          paths(search(rule)),
          paths(search(scope ?? { combine: 'all', rules: [] })),
        ])
          .then(([matches, known]) => {
            if (!current()) return;
            setLibrary({ catalogMatches: matches, catalogKnown: known });
          })
          .catch((e) => {
            // The old answer is not this filter's: show nothing and say why.
            if (!current()) return;
            setLibrary({ catalogMatches: new Set(), catalogKnown: null });
            notify.error(i18n.t('studio.library.searchFailed', { error: String(e) }));
          });
      }, delay);
    };
    ask(SEARCH_DELAY_MS);
    const stop = onCatalogChange(() => ask(SEARCH_DELAY_MS));
    return () => {
      cancelled = true;
      clearTimeout(timer);
      stop();
    };
  }, [directory, text, selection, eyes, imageList]);
}
