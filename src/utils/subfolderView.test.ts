import { describe, expect, it } from 'vitest';
import { type AppSettings, LibraryViewMode } from '../components/ui/AppProperties';
import { folderViewMode, showsSubfolders, withSubfolderView } from './subfolderView';

const photos = '/Shoots/Wedding.catalog/Photos';

describe('photos in subfolders', () => {
  it("is on for a catalog's Photos root and off elsewhere until chosen", () => {
    expect(showsSubfolders(photos, {}, photos)).toBe(true);
    expect(showsSubfolders(`${photos}/2017`, {}, photos)).toBe(false);
    expect(showsSubfolders(photos, {}, null)).toBe(false);
  });

  it('remembers the choice per folder', () => {
    let settings = withSubfolderView({} as AppSettings, photos, false);
    settings = withSubfolderView(settings, `${photos}/2017`, true);
    expect(showsSubfolders(photos, settings, photos)).toBe(false);
    expect(showsSubfolders(`${photos}/2017`, settings, photos)).toBe(true);
    expect(folderViewMode(`${photos}/2017`, settings, photos)).toBe(LibraryViewMode.Recursive);
  });

  it('keeps the inherited global recursive mode for folders without a choice', () => {
    expect(showsSubfolders('/Other', { libraryViewMode: LibraryViewMode.Recursive }, null)).toBe(true);
  });
});
