// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from 'vitest';
import type { LastFolderState } from '../components/ui/AppProperties';
import { PREVIOUS_IMPORT_VIEW_ID } from './folderSync';
import { isInside, samePath, savedView } from './restoreView';

const state = (over: Partial<LastFolderState>): LastFolderState => ({
  currentFolderPath: null,
  expandedFolders: [],
  activeAlbumId: null,
  expandedAlbumGroups: [],
  sessionDirectory: 'C:\\Users\\rawtonic\\Pictures\\catalog 3',
  ...over
});

describe('saved library view', () => {
  it('compares Windows paths across verbatim prefix, separators, case and trailing slash', () => {
    expect(samePath('\\\\?\\C:\\Users\\a\\raws 9', 'c:/users/a/raws 9/')).toBe(true);
    expect(samePath('\\\\?\\UNC\\nas\\share\\raws', '\\\\NAS\\share\\raws\\')).toBe(true);
    expect(samePath('/Users/a/Raws', '/Users/a/raws')).toBe(false);
    expect(isInside('C:\\Photos\\raws 9\\day 1', '\\\\?\\C:\\Photos')).toBe(true);
    expect(isInside('C:\\Photos2', 'C:\\Photos')).toBe(false);
  });

  it('returns to the folder it was closed on, in either spelling of the library path', () => {
    const last = state({ currentFolderPath: 'C:\\Users\\rawtonic\\raws 9' });
    const roots = ['\\\\?\\C:\\Users\\rawtonic\\Pictures\\catalog 3', '\\\\?\\C:\\Users\\rawtonic\\raws 9'];
    expect(savedView(last, '\\\\?\\C:\\Users\\rawtonic\\Pictures\\catalog 3', roots)).toEqual({
      kind: 'folder',
      path: 'C:\\Users\\rawtonic\\raws 9'
    });
  });

  it('returns to Previous Import and to a collection', () => {
    const dir = 'C:\\Users\\rawtonic\\Pictures\\catalog 3';
    expect(
      savedView(state({ currentFolderPath: 'Album: Previous Import', activeAlbumId: PREVIOUS_IMPORT_VIEW_ID }), dir, [
        dir
      ])
    ).toEqual({ kind: 'previousImport' });
    expect(
      savedView(state({ currentFolderPath: 'Album: Best', activeAlbumId: 'session-album:abc' }), dir, [dir])
    ).toEqual({ kind: 'album', id: 'abc' });
  });

  it('shows the library folder instead when the view is another library’s, unknown or transient', () => {
    const dir = 'C:\\Users\\rawtonic\\Pictures\\catalog 3';
    expect(savedView(null, dir, [dir])).toBeNull();
    expect(savedView(state({ currentFolderPath: `${dir}\\x`, sessionDirectory: 'D:\\other' }), dir, [dir])).toBeNull();
    expect(savedView(state({ currentFolderPath: `${dir}\\x`, sessionDirectory: null }), dir, [dir])).toBeNull();
    expect(savedView(state({ currentFolderPath: 'D:\\elsewhere' }), dir, [dir])).toBeNull();
    expect(
      savedView(state({ currentFolderPath: 'Album: Missing', activeAlbumId: 'session-missing' }), dir, [dir])
    ).toBeNull();
  });
});
