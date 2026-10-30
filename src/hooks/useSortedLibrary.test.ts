import { describe, expect, it } from 'vitest';
import { computeGroupedLibrary, computeSortedLibrary, type LibraryQueryState } from './useSortedLibrary';
import { type ImageFile, RawStatus, SortDirection } from '../components/ui/AppProperties';

const file = (path: string, rating = 0, tags: string[] = []): ImageFile => ({
  path, rating, tags, modified: 0, is_edited: false, is_raw: true,
  exif: null, is_virtual_copy: false, is_cloud_placeholder: false, group_id: null,
});
const files = [file('/shoot/c.ARW', 4, ['color:red']), file('/shoot/a.ARW', 5, ['color:red']), file('/shoot/b.ARW')];
const state = (): LibraryQueryState => ({
  imageList: files, imageRatings: {}, imageFlags: { '/shoot/a.ARW': 1, '/shoot/c.ARW': -1 },
  filterCriteria: { flag: 'all', rating: 0, colors: [], rawStatus: RawStatus.All },
  searchCriteria: { text: '', tags: [], mode: 'OR' },
  sortCriteria: { key: 'name', order: SortDirection.Ascending },
});

describe('live culling filters', () => {
  it('combines flags, rating and labels; counts describe the other active constraints', () => {
    const input = state();
    const result = computeGroupedLibrary({ ...input, filterCriteria: { ...input.filterCriteria, rating: 4, colors: ['red'], flag: 'picked' } }, {});
    expect(result.displayList.map((f) => f.path)).toEqual(['/shoot/a.ARW']);
    expect(result.flagCounts).toEqual({ all: 2, picked: 1, unflagged: 0, rejected: 1 });
    expect(files[0].path).toBe('/shoot/c.ARW'); // no input mutation
  });

  it('preserves an explicit zero rating over a stored rating and supports unrated', () => {
    const input = state();
    const result = computeGroupedLibrary({ ...input, imageRatings: { '/shoot/a.ARW': 0 }, filterCriteria: { ...input.filterCriteria, rating: -1 } }, {});
    expect(result.displayList.map((f) => f.path)).toEqual(['/shoot/a.ARW', '/shoot/b.ARW']);
  });

  it('uses the source flag for virtual copies and reflects a cleared flag immediately', () => {
    const input = { ...state(), imageList: [file('/shoot/a.ARW?vc=copy')] };
    input.filterCriteria.flag = 'picked';
    expect(computeSortedLibrary(input, {})).toHaveLength(1);
    expect(computeSortedLibrary({ ...input, imageFlags: { '/shoot/a.ARW': 0 } }, {})).toHaveLength(0);
  });

  it('counts matches after search, returns zero counts for no matches and reuses grouping badges', () => {
    const input = state();
    input.searchCriteria.text = 'b.ARW';
    expect(computeGroupedLibrary(input, {}).flagCounts).toEqual({ all: 1, picked: 0, unflagged: 1, rejected: 0 });
    input.searchCriteria.text = 'missing';
    expect(computeGroupedLibrary(input, {}).flagCounts.all).toBe(0);
    const grouped = computeGroupedLibrary({ ...state(), imageList: [
      { ...file('/shoot/a.ARW'), group_id: 'a' },
      { ...file('/shoot/a.jpg'), is_raw: false, group_id: 'a' },
    ] }, { appSettings: { grouping: 'raw' } });
    expect(grouped.displayList).toHaveLength(1);
    expect(grouped.flagCounts.picked).toBe(1);
    expect(grouped.badges?.get('a')?.count).toBe(2);
  });
});

describe('catalog search', () => {
  it('shows what the library found, virtual copies with their photograph, culling filters on top', () => {
    const input = {
      ...state(),
      imageList: [...files, file('/shoot/a.ARW?vc=copy', 5)],
      // The catalog matched a keyword the grid does not know (no text match).
      searchCriteria: { text: 'wedding', tags: [], mode: 'OR' as const },
      catalogMatches: new Set(['/shoot/a.ARW', '/shoot/b.ARW']),
    };
    const shown = computeGroupedLibrary(input, {});
    expect(shown.displayList.map((f) => f.path)).toEqual(['/shoot/a.ARW', '/shoot/a.ARW?vc=copy', '/shoot/b.ARW']);
    expect(shown.flagCounts.all).toBe(3);
    input.filterCriteria = { ...input.filterCriteria, rating: 5 };
    expect(computeSortedLibrary(input, {}).map((f) => f.path)).toEqual(['/shoot/a.ARW', '/shoot/a.ARW?vc=copy']);
    // An empty answer shows nothing; before the first answer names match here.
    expect(computeSortedLibrary({ ...input, catalogMatches: new Set() }, {})).toEqual([]);
    expect(computeSortedLibrary({ ...state(), searchCriteria: { text: 'c.ARW', tags: [], mode: 'OR' }, catalogMatches: null }, {})
      .map((f) => f.path)).toEqual(['/shoot/c.ARW']);
  });

  it('finds a virtual copy as its photograph (its own keywords are not indexed: G37)', () => {
    const copy = { ...file('/shoot/a.ARW?vc=c1', 0, ['user:client']), is_virtual_copy: true };
    const input = {
      ...state(),
      imageList: [file('/shoot/a.ARW', 0, ['user:client']), copy, file('/shoot/b.ARW')],
      searchCriteria: { text: 'client', tags: [], mode: 'OR' as const },
      catalogMatches: new Set(['/shoot/a.ARW']),
      catalogKnown: new Set(['/shoot/a.ARW', '/shoot/b.ARW']),
    };
    expect(computeSortedLibrary(input, {}).map((f) => f.path)).toEqual(['/shoot/a.ARW', '/shoot/a.ARW?vc=c1']);
  });

  it('matches files the catalog does not hold by name and tags', () => {
    const input = {
      ...state(),
      imageList: [file('/shoot/a.ARW'), file('/shoot/Output/DSC00123.JPG')],
      searchCriteria: { text: '00123', tags: [], mode: 'OR' as const },
      catalogMatches: new Set<string>(),
      catalogKnown: new Set(['/shoot/a.ARW']),
    };
    expect(computeSortedLibrary(input, {}).map((f) => f.path)).toEqual(['/shoot/Output/DSC00123.JPG']);
    // It has no camera, lens, …: a metadata choice leaves it out.
    expect(
      computeSortedLibrary({ ...input, filterCriteria: { ...input.filterCriteria, metadata: { camera: ['X-T4'] } } }, {}),
    ).toEqual([]);
  });

  it('keeps grouped photographs together when one of them is found', () => {
    const grouped = computeGroupedLibrary({
      ...state(),
      imageList: [{ ...file('/shoot/a.ARW'), group_id: 'a' }, { ...file('/shoot/a.jpg'), is_raw: false, group_id: 'a' }],
      catalogMatches: new Set(['/shoot/a.jpg']),
    }, { appSettings: { grouping: 'raw' } });
    expect(grouped.displayList).toHaveLength(1);
  });
});

describe('filename sorting', () => {
  it('preserves locale ordering, Windows paths, direction and ascending ties', () => {
    const input = { ...state(), imageList: ['z.ARW', 'ä.ARW', 'A.ARW', 'IMG_10.ARW', 'IMG_2.ARW'].map((name) => file(`C:\\shoot\\${name}`)) };
    const expected = [...input.imageList].sort((a, b) => a.path.split('\\').pop()!.localeCompare(b.path.split('\\').pop()!));
    expect(computeSortedLibrary(input, {})).toEqual(expected);
    input.sortCriteria.order = SortDirection.Descending;
    expect(computeSortedLibrary(input, {})).toEqual([...expected].reverse());
    input.sortCriteria.key = 'rating';
    expect(computeSortedLibrary(input, {})).toEqual(expected);
  });

  it('reuses ordering across live culling changes without returning stale matches or counts', () => {
    const initial = state();
    computeGroupedLibrary(initial, {});
    const rated = { ...initial, imageRatings: { '/shoot/b.ARW': 5 }, filterCriteria: { ...initial.filterCriteria, rating: 5 } };
    expect(computeSortedLibrary(rated, {}).map((f) => f.path)).toEqual(['/shoot/a.ARW', '/shoot/b.ARW']);
    const picked = { ...rated, imageFlags: { '/shoot/b.ARW': 1 }, filterCriteria: { ...rated.filterCriteria, flag: 'picked' as const } };
    const result = computeGroupedLibrary(picked, {});
    expect(result.displayList.map((f) => f.path)).toEqual(['/shoot/b.ARW']);
    expect(result.flagCounts).toEqual({ all: 2, picked: 1, unflagged: 1, rejected: 0 });
    expect(computeSortedLibrary({ ...picked, searchCriteria: { ...picked.searchCriteria, text: 'a.ARW' } }, {})).toEqual([]);
  });

  it('invalidates rating order, replaced metadata records and changed direction', () => {
    const input = { ...state(), sortCriteria: { key: 'rating' as const, order: SortDirection.Descending } };
    expect(computeSortedLibrary(input, {}).map((f) => f.path)).toEqual(['/shoot/a.ARW', '/shoot/c.ARW', '/shoot/b.ARW']);
    const rated = { ...input, imageRatings: { '/shoot/b.ARW': 5 } };
    expect(computeSortedLibrary(rated, {}).map((f) => f.path)).toEqual(['/shoot/a.ARW', '/shoot/b.ARW', '/shoot/c.ARW']);
    const byIso = { ...rated, sortCriteria: { key: 'iso' as const, order: SortDirection.Descending } };
    expect(computeSortedLibrary(byIso, {}).map((f) => f.path)).toEqual(['/shoot/a.ARW', '/shoot/b.ARW', '/shoot/c.ARW']);
    const hydrated = { ...byIso, imageList: byIso.imageList.map((f) => f.path.includes('c.ARW') ? { ...f, exif: { PhotographicSensitivity: '800' } } : f) };
    expect(computeSortedLibrary(hydrated, {}).map((f) => f.path)).toEqual(['/shoot/c.ARW', '/shoot/a.ARW', '/shoot/b.ARW']);
    expect(computeSortedLibrary({ ...hydrated, sortCriteria: { ...hydrated.sortCriteria, order: SortDirection.Ascending } }, {}).map((f) => f.path)).toEqual(['/shoot/a.ARW', '/shoot/b.ARW', '/shoot/c.ARW']);
  });
});

describe('capture-time order', () => {
  it('sorts a burst within one second by its fraction, as corrected', () => {
    const shot = (path: string, exif: Record<string, string>) => ({ ...file(path), exif });
    const input = {
      ...state(),
      imageList: [
        shot('/shoot/b.ARW', { DateTimeOriginal: '2026-10-30 12:00:00', SubSecTimeOriginal: '5' }),
        shot('/shoot/a.ARW', { DateTimeOriginal: '2026-10-30 12:00:00', SubSecTimeOriginal: '45' }),
        // Corrected from another camera: a later second, no fraction.
        shot('/shoot/c.CR3', { DateTimeOriginal: '2026-10-30 12:00:01' }),
        shot('/shoot/d.CR3', { DateTimeOriginal: '2026-10-30 11:59:59', SubSecTimeOriginal: '999' }),
      ],
      sortCriteria: { key: 'date_taken', order: SortDirection.Ascending },
    };
    expect(computeSortedLibrary(input, {}).map((f) => f.path)).toEqual([
      '/shoot/d.CR3', '/shoot/a.ARW', '/shoot/b.ARW', '/shoot/c.CR3',
    ]);
  });
});
