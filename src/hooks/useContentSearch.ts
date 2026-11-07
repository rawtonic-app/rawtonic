// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import i18n from 'i18next';
import { notify } from '../activity/notify';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { errorCode, errorMessage } from '../shared/ipc/errors';
import { photographPath } from '../utils/librarySearch';
import {
  CONTENT_INDEX_DONE,
  CONTENT_INDEX_PROGRESS,
  contentSearchStatus,
  downloadContentSearch,
  rankByContent,
  readSearchMode,
  startContentIndex,
  stopContentIndex,
  writeSearchMode,
  type ContentIndexProgress,
  type SearchMode
} from '../utils/contentSearch';

/** How long typing pauses before the text model is asked (it takes ~10–20
 * ms a query, the first one loads it). */
export const CONTENT_DELAY_MS = 250;
/** How long list changes settle before the pass is asked to start again. */
const RESTART_DELAY_MS = 2000;
/** How often the open library is asked for photographs not indexed yet. */
export const CONTENT_BACKLOG_MS = 30_000;

/** Reads the model and index state of the open library into the store. */
export async function refreshContentStatus(directory: string) {
  const status = await contentSearchStatus(directory);
  if (useLibraryStore.getState().sessionDirectory === directory)
    useLibraryStore.getState().setLibrary({ contentStatus: status });
  return status;
}

/** The search box's mode, remembered for the open library. */
export function setSearchMode(mode: SearchMode) {
  const { sessionDirectory, setLibrary } = useLibraryStore.getState();
  writeSearchMode(sessionDirectory, mode);
  setLibrary({ searchMode: mode, ...(mode === 'text' ? { contentRanking: null } : {}) });
  if (mode === 'content' && sessionDirectory) refreshContentStatus(sessionDirectory).catch(() => {});
}

/** The photographer clicked "Download search model": turns content search
 * on for every library and downloads the model (PRIVACY.md), then indexes
 * the open library. */
export async function downloadSearchModel(directory: string) {
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (appSettings && !appSettings.contentSearch) await handleSettingsChange({ ...appSettings, contentSearch: true });
  try {
    await downloadContentSearch(directory);
  } catch (error) {
    notify.error(i18n.t('studio.contentSearch.downloadFailed', { error: errorMessage(error) }));
  }
  await refreshContentStatus(directory).catch(() => {});
}

/**
 * Content search in the open library: takes the search box's mode remembered
 * for it, runs the background index while the setting is on (never
 * downloading), and in Content mode ranks the library for the text, which
 * `useSortedLibrary` turns into the view's order. Outside a library there is
 * no Content mode.
 */
export function useContentSearch() {
  const directory = useLibraryStore((s) => s.sessionDirectory);
  const mode = useLibraryStore((s) => s.searchMode);
  const text = useLibraryStore((s) => s.searchCriteria.text);
  const imageList = useLibraryStore((s) => s.imageList);
  const enabled = useSettingsStore((s) => !!s.appSettings?.contentSearch);

  // Each library has its own mode; a new library starts without a ranking.
  useEffect(() => {
    const searchMode = directory ? readSearchMode(directory) : 'text';
    useLibraryStore.getState().setLibrary({ searchMode, contentRanking: null, contentStatus: null });
    if (directory && searchMode === 'content') refreshContentStatus(directory).catch(() => {});
  }, [directory]);

  // The index pass of this library while the setting is on; it stops when
  // the library closes or the setting goes off.
  useEffect(() => {
    if (!directory || !enabled) return;
    startContentIndex(directory, true).catch(() => {});
    return () => {
      stopContentIndex(directory).catch(() => {});
    };
  }, [directory, enabled]);
  useEffect(() => {
    if (!directory || !enabled) return;
    const timer = setInterval(() => startContentIndex(directory, false).catch(() => {}), CONTENT_BACKLOG_MS);
    return () => clearInterval(timer);
  }, [directory, enabled]);
  const firstList = useRef(true);
  useEffect(() => {
    if (firstList.current) {
      firstList.current = false;
      return;
    }
    if (!directory || !enabled) return;
    const timer = setTimeout(() => startContentIndex(directory, false).catch(() => {}), RESTART_DELAY_MS);
    return () => clearTimeout(timer);
    // `directory` and `enabled` start it above; only a new list asks here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [imageList]);

  // The index's progress reaches the status ("Indexed N of M"); a pass
  // that finished ranks the shown text again with what it read.
  const [finished, setFinished] = useState(0);
  useEffect(() => {
    if (!directory) return;
    let gone = false;
    const stops: (() => void)[] = [];
    const update = ({ directory: library, indexed, failed, total }: ContentIndexProgress, running: boolean) => {
      const { sessionDirectory, contentStatus, setLibrary } = useLibraryStore.getState();
      if (library !== sessionDirectory || !contentStatus) return;
      setLibrary({ contentStatus: { ...contentStatus, indexed, failed, total, running } });
    };
    listen<ContentIndexProgress>(CONTENT_INDEX_PROGRESS, (e) => update(e.payload, true))
      .then((stop) => (gone ? stop() : stops.push(stop)))
      .catch(() => {});
    listen<ContentIndexProgress>(CONTENT_INDEX_DONE, (e) => {
      update(e.payload, false);
      if (e.payload.directory === useLibraryStore.getState().sessionDirectory) setFinished((n) => n + 1);
    })
      .then((stop) => (gone ? stop() : stops.push(stop)))
      .catch(() => {});
    return () => {
      gone = true;
      for (const stop of stops) stop();
    };
  }, [directory]);

  // Content mode with text: the library ranked for it. Only the newest
  // request publishes; a failed one shows the view in its own order and
  // says why (without the model the switch offers the download instead).
  const query = mode === 'content' ? text.trim() : '';
  useEffect(() => {
    if (!directory || !query) {
      if (useLibraryStore.getState().contentRanking) useLibraryStore.getState().setLibrary({ contentRanking: null });
      return;
    }
    let cancelled = false;
    const { currentFolderPath, activeAlbumId } = useLibraryStore.getState();
    const listed = activeAlbumId ? null : currentFolderPath;
    const timer = setTimeout(() => {
      rankByContent(directory, query, listed)
        .then((answer) => {
          if (cancelled || useLibraryStore.getState().sessionDirectory !== directory) return;
          const ranks = new Map<string, number>();
          answer.paths.forEach((path, place) => {
            const photo = photographPath(path);
            if (!ranks.has(photo)) ranks.set(photo, place);
          });
          const { contentStatus, setLibrary } = useLibraryStore.getState();
          setLibrary({
            contentRanking: { query, ranks },
            contentStatus: contentStatus && {
              ...contentStatus,
              indexed: answer.indexed,
              failed: answer.failed,
              total: answer.total
            }
          });
        })
        .catch((error) => {
          if (cancelled) return;
          useLibraryStore.getState().setLibrary({ contentRanking: null });
          if (errorCode(error) === 'models_missing') refreshContentStatus(directory).catch(() => {});
          else notify.error(i18n.t('studio.contentSearch.searchFailed', { error: errorMessage(error) }));
        });
    }, CONTENT_DELAY_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
    // The folder or album shown is read when asked: the ranking covers the
    // whole library, so another view sorts by the same answer.
  }, [directory, query, finished]);
}
