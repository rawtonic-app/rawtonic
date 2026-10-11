import { invoke } from '@tauri-apps/api/core';
import type { ImageFile } from '../components/ui/AppProperties';
import { performanceDiagnosticsEnabled, reportPerformance } from '../hooks/usePerformanceDiagnostics';

const publications = new WeakMap<ImageFile[], { loaded: number; published: number }>();
const loadStarts = new WeakMap<ImageFile[], number>();

export function markLibraryPublication(files: ImageFile[]) {
  if (!performanceDiagnosticsEnabled) return;
  publications.set(files, { loaded: loadStarts.get(files) ?? performance.now(), published: performance.now() });
}

export function reportLibraryPresentation(files: ImageFile[], visible: number) {
  const times = publications.get(files);
  if (!times) return;
  publications.delete(files);
  reportPerformance('library_first_grid_frame', {
    ms: performance.now() - times.loaded,
    publish_to_frame_ms: performance.now() - times.published,
    assets: files.length,
    visible,
  });
}

interface ListingPage {
  listing_id: string | null;
  offset: number;
  total: number;
  files: ImageFile[];
}

export function abortLibraryLoad(): never {
  throw new DOMException('Library navigation was superseded', 'AbortError');
}

// One immutable snapshot is published only after every page arrives. Filters,
// grouping and range selection never act on a temporarily incomplete library.
// Each IPC response is bounded; the native scan runs off the UI thread.
export async function loadLibraryImages(path: string, recursive: boolean, signal?: AbortSignal): Promise<ImageFile[]> {
  if (signal?.aborted) abortLibraryLoad();
  const started = performanceDiagnosticsEnabled ? performance.now() : 0;
  let listingId: string | null = null;
  let pages = 0;
  try {
    let page = await invoke<ListingPage>('begin_image_listing', { path, recursive });
    listingId = page.listing_id;
    const firstPageMs = performanceDiagnosticsEnabled ? performance.now() - started : 0;
    const files: ImageFile[] = [];
    const total = page.total;
    while (true) {
      if (signal?.aborted) abortLibraryLoad();
      if (page.total !== total || page.offset !== files.length || page.files.length > 512
          || files.length + page.files.length > total || (page.listing_id && !page.files.length)) {
        throw new Error('Invalid library listing page');
      }
      files.push(...page.files);
      pages++;
      if (!page.listing_id) {
        if (files.length !== total) throw new Error('Incomplete library listing');
        listingId = null;
        break;
      }
      page = await invoke<ListingPage>('next_image_listing_page', { listingId, offset: files.length });
    }
    if (performanceDiagnosticsEnabled) reportPerformance('library_listing', {
      ms: performance.now() - started, first_page_ms: firstPageMs, assets: total, pages,
    });
    if (performanceDiagnosticsEnabled) loadStarts.set(files, started);
    return files;
  } finally {
    // This also handles cancellation while the initial scan was still running.
    if (listingId) await invoke('cancel_image_listing', { listingId }).catch(() => {});
  }
}
