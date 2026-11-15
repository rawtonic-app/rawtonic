import { describe, expect, it } from 'vitest';
import {
  buildKeywordTree,
  countKeywordPaths,
  displayKeyword,
  isWithinKeyword,
  legacyHierarchy,
  movedKeyword,
  parseKeywordEntries,
  parseKeywordPath,
  renameKeyword,
  renameKeywordTags,
  resolveKeyword,
  tagsHaveKeyword,
  treeRows
} from './keywordTree';

describe('typed keyword paths', () => {
  it('reads the three entry forms as one hierarchy', () => {
    // Lightroom's display form, Capture One's / XMP's form, Lightroom's shortcut.
    expect(parseKeywordPath('Animals > Dogs')).toEqual(['Animals', 'Dogs']);
    expect(parseKeywordPath('Animals|Dogs')).toEqual(['Animals', 'Dogs']);
    expect(parseKeywordPath('Dogs < Animals')).toEqual(['Animals', 'Dogs']);
    expect(parseKeywordPath('Beagle < Dogs < Animals')).toEqual(['Animals', 'Dogs', 'Beagle']);
    expect(parseKeywordPath(' places > europe | alps ')).toEqual(['places', 'europe', 'alps']);
    expect(parseKeywordPath('  holiday ')).toEqual(['holiday']);
    expect(parseKeywordPath(' > | ')).toEqual([]);
  });

  it('splits entries at commas and drops duplicates ignoring case', () => {
    expect(parseKeywordEntries('holiday, Animals > Dogs, dogs < animals,, travel')).toEqual([
      ['holiday'],
      ['Animals', 'Dogs'],
      ['travel']
    ]);
  });

  it('spells a path as the library does and finds a unique name in the tree', () => {
    const existing = ['Animals|Dogs', 'Animals|Cats', 'Places|Dogs Bay', 'beach'];
    expect(resolveKeyword(['animals', 'dogs'], existing)).toBe('Animals|Dogs');
    expect(resolveKeyword(['ANIMALS', 'Birds'], existing)).toBe('Animals|birds');
    // A single name: the one keyword of that name in the tree…
    expect(resolveKeyword(['cats'], existing)).toBe('Animals|Cats');
    // …a top-level keyword by the field's rule (lowercase, as before)…
    expect(resolveKeyword(['Beach'], existing)).toBe('beach');
    expect(resolveKeyword(['Harbour'], existing)).toBe('harbour');
    // …and a parent typed alone is that top-level keyword.
    expect(resolveKeyword(['Animals'], existing)).toBe('animals');
    // Two keywords of that name: none is guessed.
    expect(resolveKeyword(['dogs'], [...existing, 'Pets|Dogs'])).toBe('dogs');
  });
});

describe('keyword tree', () => {
  it('matches a parent with everything under it, level by level', () => {
    expect(isWithinKeyword('Animals|Dogs', 'animals')).toBe(true);
    expect(isWithinKeyword('Animals|Dogs', 'Animals|Dogs')).toBe(true);
    expect(isWithinKeyword('Animalsx|Dogs', 'Animals')).toBe(false);
    expect(isWithinKeyword('Animals', 'Animals|Dogs')).toBe(false);
    expect(tagsHaveKeyword(['color:red', 'user:Animals|Dogs'], 'Animals')).toBe(true);
    expect(tagsHaveKeyword(['user:places > asia'], 'places')).toBe(false);
  });

  it('builds the tree with counts, a missing parent counting its largest child', () => {
    const tree = buildKeywordTree([
      { value: 'Animals', count: 3 },
      { value: 'Animals|Dogs', count: 2 },
      { value: 'Animals|Cats', count: 2 },
      { value: 'Places|Europe|Alps', count: 5 },
      { value: 'holiday', count: 7 }
    ]);
    expect(tree.map((n) => [n.name, n.count])).toEqual([
      ['Animals', 3],
      ['holiday', 7],
      ['Places', 5]
    ]);
    expect(tree[0].children.map((n) => n.path)).toEqual(['Animals|Cats', 'Animals|Dogs']);
    expect(tree[2].children[0].children[0]).toMatchObject({ path: 'Places|Europe|Alps', count: 5 });
  });

  it('counts each path and its parents once per photograph from tags', () => {
    const counts = countKeywordPaths([['Animals|Dogs', 'Animals|Cats'], ['animals|dogs'], ['beach']]);
    const of = (value: string) => counts.find((c) => c.value.toLowerCase() === value.toLowerCase())?.count;
    expect(of('Animals')).toBe(2);
    expect(of('Animals|Dogs')).toBe(2);
    expect(of('Animals|Cats')).toBe(1);
    expect(of('beach')).toBe(1);
  });

  it('lists value rows in tree order with their depth, no keyword last', () => {
    const rows = treeRows([
      { value: '', count: 4 },
      { value: 'Animals|Dogs', count: 2 },
      { value: 'beach', count: 9 },
      { value: 'Animals', count: 3 }
    ]);
    expect(rows.map(({ row, depth }) => [row.value, depth])).toEqual([
      ['Animals', 0],
      ['Animals|Dogs', 1],
      ['beach', 0],
      ['', 0]
    ]);
  });

  it('renames, moves and converts paths with the keywords under them', () => {
    expect(renameKeyword('Animals|Dogs|Beagle', 'animals', 'Fauna')).toBe('Fauna|Dogs|Beagle');
    expect(renameKeyword('Animalsx', 'Animals', 'Fauna')).toBe('Animalsx');
    expect(movedKeyword('Dogs', 'Animals')).toBe('Animals|Dogs');
    expect(movedKeyword('Animals|Dogs', null)).toBe('Dogs');
    // Nowhere new: onto itself, below itself, its own parent, the top it is at.
    expect(movedKeyword('Animals', 'Animals|Dogs')).toBeNull();
    expect(movedKeyword('Animals|Dogs', 'animals')).toBeNull();
    expect(movedKeyword('Animals', null)).toBeNull();
    expect(legacyHierarchy('places > asia > japan')).toBe('places|asia|japan');
    expect(legacyHierarchy('japan < asia')).toBe('asia|japan');
    expect(legacyHierarchy('Animals|Dogs')).toBeNull();
    expect(legacyHierarchy('beach')).toBeNull();
    expect(displayKeyword('Animals|Dogs')).toBe('Animals > Dogs');
  });

  it('renames tags as the backend does: merges once, keeps the user marker', () => {
    expect(
      renameKeywordTags(
        ['user:Animals', 'user:animals|Dogs|Beagle', 'Animals|Cats', 'color:red', 'user:Fauna|cats'],
        'Animals',
        'Fauna'
      )
    ).toEqual(['color:red', 'user:Fauna|cats', 'user:Fauna', 'user:Fauna|Dogs|Beagle']);
    expect(renameKeywordTags(['Fauna', 'user:Animals'], 'Animals', 'fauna')).toEqual(['user:Fauna']);
  });
});
