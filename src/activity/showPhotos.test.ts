import { beforeEach, expect, it, vi } from 'vitest';
import { SHOW_ACTIVITY_PHOTOS_EVENT, showPhotos, shownActivityPhotos } from './showPhotos';
import { resetActivityStore, useActivityStore } from '../store/useActivityStore';
import { followRenamedPaths } from '../utils/pathKeyedState';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

beforeEach(() => resetActivityStore());

it('asks the app for the view and keeps its photographs, following a rename', () => {
  const asked = vi.fn();
  window.addEventListener(SHOW_ACTIVITY_PHOTOS_EVENT, asked);
  useActivityStore.getState().report('failure', { id: 'f', source: 'export', title: '1 photo did not export', paths: ['/s/A.NEF'] });
  showPhotos('1 photo did not export', ['/s/A.NEF', '/s/B.NEF']);
  expect((asked.mock.calls[0][0] as CustomEvent).detail).toEqual({ name: '1 photo did not export', paths: ['/s/A.NEF', '/s/B.NEF'] });
  window.removeEventListener(SHOW_ACTIVITY_PHOTOS_EVENT, asked);

  followRenamedPaths(['/s/A.NEF'], ['/s/Wedding-001.NEF']);
  // Listed again on refresh at the new place; the entry's Show photos too.
  expect(shownActivityPhotos()?.paths).toEqual(['/s/Wedding-001.NEF', '/s/B.NEF']);
  expect(useActivityStore.getState().entries[0].paths).toEqual(['/s/Wedding-001.NEF']);
});
