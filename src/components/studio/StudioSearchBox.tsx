// SPDX-License-Identifier: AGPL-3.0-or-later
import { useState, type RefObject } from 'react';
import { useTranslation } from 'react-i18next';
import { useShallow } from 'zustand/react/shallow';
import { Search } from 'lucide-react';
import { useLibraryStore } from '../../store/useLibraryStore';
import { downloadSearchModel, setSearchMode } from '../../hooks/useContentSearch';
import { downloadSize } from '../../utils/contentSearch';
import { photographPath } from '../../utils/librarySearch';
import type { ImageFile } from '../ui/AppProperties';

/**
 * The Library's search box: in a library its mode switch (Text: words in
 * names, keywords and metadata, a filter; Content: what the photographs
 * show, an order of the view, G37) and, in Content mode, the search model's
 * download or how far the index is. `files`: the photographs shown, for the
 * note on those not indexed yet.
 */
export default function StudioSearchBox({
  inputRef,
  files
}: {
  inputRef: RefObject<HTMLInputElement | null>;
  files: ImageFile[];
}) {
  const { t, i18n } = useTranslation();
  const { directory, text, mode, status, ranking } = useLibraryStore(
    useShallow((s) => ({
      directory: s.sessionDirectory,
      text: s.searchCriteria.text,
      mode: s.searchMode,
      status: s.contentStatus,
      ranking: s.contentRanking
    }))
  );
  const [downloading, setDownloading] = useState(false);
  const content = !!directory && mode === 'content';
  const number = (n: number) => n.toLocaleString(i18n.language);
  const unranked = ranking ? files.filter((file) => !ranking.ranks.has(photographPath(file.path))).length : 0;

  let detail = null;
  if (content && status && !status.installed) {
    detail = (
      <button
        className="studio-search-download"
        title={t('studio.contentSearch.downloadTitle')}
        disabled={downloading}
        onClick={() => {
          setDownloading(true);
          void downloadSearchModel(directory).finally(() => setDownloading(false));
        }}
      >
        {t('studio.contentSearch.download', { size: downloadSize(status.downloadBytes, i18n.language) })}
      </button>
    );
  } else if (content && ranking && unranked > 0) {
    detail = (
      <span className="studio-search-detail" role="status">
        {t('studio.contentSearch.notIndexed', { count: unranked })}
      </span>
    );
  } else if (content && status && status.indexed + status.failed < status.total) {
    detail = (
      <span className="studio-search-detail" role="status">
        {t('studio.contentSearch.indexed', { indexed: number(status.indexed), total: number(status.total) })}
      </span>
    );
  }

  return (
    <div className="studio-search-group">
      {directory && (
        <div className="studio-search-mode" role="group" aria-label={t('studio.contentSearch.modeLabel')}>
          <button
            aria-pressed={mode === 'text'}
            title={t('studio.contentSearch.textTitle')}
            onClick={() => setSearchMode('text')}
          >
            {t('studio.contentSearch.text')}
          </button>
          <button
            aria-pressed={mode === 'content'}
            title={t('studio.contentSearch.contentTitle')}
            onClick={() => setSearchMode('content')}
          >
            {t('studio.contentSearch.content')}
          </button>
        </div>
      )}
      <label className="studio-search">
        <Search size={13} />
        <input
          ref={inputRef}
          type="search"
          aria-label={t('studio.workspace.grid.searchLabel')}
          placeholder={content ? t('studio.contentSearch.placeholder') : t('studio.workspace.grid.search')}
          value={text}
          onChange={(e) => useLibraryStore.getState().setSearchCriteria({ text: e.target.value })}
        />
      </label>
      {detail}
    </div>
  );
}
