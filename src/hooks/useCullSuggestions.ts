// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useCullStore } from '../store/useCullStore';
import { useEyesStore } from '../store/useEyesStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { CULLING_CHANGED } from './useCatalogSearch';

/** How long list changes settle before the suggestions are read again. */
const LOAD_DELAY_MS = 300;

/**
 * Keeps the Culling panel's suggestions (`useCullStore`) to the listed
 * photographs of the open library: read again when the list changes, when
 * the content pass scored photographs or a suggestion was dismissed
 * (`library-culling-changed`), and when eyes were read or called. Reading
 * only: the background passes that score are content search's and the eye
 * check's.
 */
export function useCullSuggestions() {
  const directory = useLibraryStore((s) => s.sessionDirectory);
  const imageList = useLibraryStore((s) => s.imageList);
  const generation = useCullStore((s) => s.generation);
  const eyes = useEyesStore((s) => s.generation);

  useEffect(() => {
    if (!directory) return;
    let gone = false;
    let stop: (() => void) | undefined;
    listen(CULLING_CHANGED, () => useCullStore.getState().changed())
      .then((unlisten) => (gone ? unlisten() : (stop = unlisten)))
      .catch(() => {});
    return () => {
      gone = true;
      stop?.();
    };
  }, [directory]);

  useEffect(() => {
    const store = useCullStore.getState();
    if (!directory) {
      store.reset();
      return;
    }
    const timer = setTimeout(() => {
      store
        .load(
          directory,
          imageList.map((image) => image.path)
        )
        .catch(() => {});
    }, LOAD_DELAY_MS);
    return () => clearTimeout(timer);
  }, [directory, imageList, generation, eyes]);
}
