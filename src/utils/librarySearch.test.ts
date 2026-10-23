import { describe, expect, it, vi } from 'vitest';
import { EditedStatus, LibraryViewMode, RawStatus } from '../components/ui/AppProperties';
import {
  cullingRules,
  filterAsRule,
  photographPath,
  scopeRule,
  searchRule,
  selectionCount,
  selectionRules,
} from './librarySearch';
import { MISSING_VIEW_ID } from './offline';
import { PREVIOUS_IMPORT_VIEW_ID } from './folderSync';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

describe('library search rules', () => {
  it('chooses any value of a list and every list', () => {
    const rules = selectionRules({ camera: ['ILCE-7RM4', 'X-T4'], iso: ['3200'], lens: [] });
    expect(rules).toEqual([
      {
        combine: 'any',
        rules: [
          { field: 'camera', op: 'is', value: 'ILCE-7RM4' },
          { field: 'camera', op: 'is', value: 'X-T4' },
        ],
      },
      { field: 'iso', op: 'is', value: 3200 },
    ]);
    // A list's own counts leave out its own choice.
    expect(selectionRules({ camera: ['X-T4'], iso: ['3200'] }, 'camera')).toEqual([
      { field: 'iso', op: 'is', value: 3200 },
    ]);
    expect(selectionCount({ camera: ['a', 'b'], keyword: ['c'] })).toBe(3);
  });

  it('turns months and missing values into conditions', () => {
    expect(selectionRules({ captureDate: ['2024-02'] })).toEqual([
      { field: 'captureDate', op: 'between', value: ['2024-02-01', '2024-02-29'] },
    ]);
    expect(selectionRules({ captureDate: ['2024'] })).toEqual([
      { field: 'captureDate', op: 'between', value: ['2024-01-01', '2024-12-31'] },
    ]);
    expect(selectionRules({ captureDate: ['2023-12'] })).toEqual([
      { field: 'captureDate', op: 'between', value: ['2023-12-01', '2023-12-31'] },
    ]);
    expect(selectionRules({ keyword: [''] })).toEqual([{ field: 'keyword', op: 'empty' }]);
    expect(selectionRules({ fileType: ['jpeg'] })).toEqual([{ field: 'fileType', op: 'is', value: 'jpeg' }]);
    expect(selectionRules({ aperture: ['2.8'] })).toEqual([{ field: 'aperture', op: 'is', value: 2.8 }]);
  });

  it('writes the culling filters as the filter bar applies them', () => {
    const filter = { colors: [], rating: 0, rawStatus: RawStatus.All };
    expect(cullingRules(filter)).toEqual([]);
    expect(cullingRules({ ...filter, rating: -1 })).toEqual([{ field: 'rating', op: 'empty' }]);
    expect(cullingRules({ ...filter, rating: 5 })).toEqual([{ field: 'rating', op: 'is', value: 5 }]);
    expect(cullingRules({ ...filter, rating: 3, colors: ['red', 'none'], flag: 'picked' })).toEqual([
      { field: 'rating', op: 'ge', value: 3 },
      { field: 'label', op: 'is', value: ['red', 'none'] },
      { field: 'flag', op: 'is', value: 'picked' },
    ]);
  });

  it('scopes to what the grid shows', () => {
    expect(scopeRule('/job/Capture', null, LibraryViewMode.Flat)).toEqual({
      field: 'folder',
      op: 'is',
      value: '/job/Capture',
    });
    expect(scopeRule('/job/Capture', null, LibraryViewMode.Recursive)).toMatchObject({ op: 'contains' });
    expect(scopeRule('Album: Picks', 'session-album:p', undefined)).toEqual({ field: 'album', op: 'is', value: 'p' });
    expect(scopeRule(null, PREVIOUS_IMPORT_VIEW_ID, undefined)).toEqual({
      field: 'previousImport',
      op: 'is',
      value: true,
    });
    expect(scopeRule(null, MISSING_VIEW_ID, undefined)).toEqual({ field: 'offline', op: 'is', value: true });
    // The app-wide albums outside a library have no catalog.
    expect(scopeRule('Album: x', 'local-album-id', undefined)).toBeNull();
  });

  it('searches only when something is asked and saves the whole filter as one rule', () => {
    expect(searchRule('  ', {})).toBeNull();
    expect(searchRule('bride', { camera: ['X-T4'] })).toEqual({
      combine: 'all',
      rules: [
        { field: 'text', op: 'contains', value: 'bride' },
        { field: 'camera', op: 'is', value: 'X-T4' },
      ],
    });
    expect(searchRule('bride', undefined)).toEqual({ field: 'text', op: 'contains', value: 'bride' });
    const scope = scopeRule('/job/Capture', null, LibraryViewMode.Flat);
    expect(
      filterAsRule(scope, 'bride', { lens: ['FE 85mm F1.4 GM'] }, { colors: [], rating: 4, rawStatus: RawStatus.All }),
    ).toEqual({
      combine: 'all',
      rules: [
        scope,
        { field: 'text', op: 'contains', value: 'bride' },
        { field: 'lens', op: 'is', value: 'FE 85mm F1.4 GM' },
        { field: 'rating', op: 'ge', value: 4 },
      ],
    });
  });

  it('saves RAW / not RAW and refuses filters rules cannot say', () => {
    const filter = { colors: [], rating: 0, rawStatus: RawStatus.RawOnly };
    expect(filterAsRule(null, '', undefined, filter)?.rules).toEqual([
      { field: 'fileType', op: 'is', value: ['raw', 'dng'] },
    ]);
    expect(filterAsRule(null, '', undefined, { ...filter, rawStatus: RawStatus.NonRawOnly })?.rules).toEqual([
      { field: 'fileType', op: 'isNot', value: ['raw', 'dng'] },
    ]);
    expect(filterAsRule(null, '', undefined, { ...filter, editedStatus: EditedStatus.EditedOnly })).toBeNull();
    expect(filterAsRule(null, '', undefined, filter, ['iso>800'])).toBeNull();
  });

  it('finds a virtual copy by its photograph', () => {
    expect(photographPath('/job/Capture/a.arw?vc=1f2e')).toBe('/job/Capture/a.arw');
    expect(photographPath('/job/Capture/a.arw')).toBe('/job/Capture/a.arw');
  });
});
