import { describe, expect, it } from 'vitest';
import {
  buildKeywordTree,
  countKeywordPaths,
  deleteKeywordTags,
  displayKeyword,
  isWithinKeyword,
  keywordWithout,
  legacyHierarchy,
  movedKeyword,
  parseKeywordEntries,
  parseKeywordPath,
  renameKeyword,
  renameKeywordTags,
  resolveKeyword,
  resolveKeywordEntries,
  tagsHaveKeyword,
  treeRows,
  withDeclaredKeywords
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

describe('field entries', () => {
  it('keeps an existing flat “a > b” keyword when it is entered as it is stored', () => {
    const existing = ['places > asia', 'Animals|Dogs'];
    // As autocomplete offers it: the stored keyword, not a new hierarchy.
    expect(resolveKeywordEntries('Places > Asia', existing)).toEqual(['places > asia']);
    // Anything else with `>` is a path.
    expect(resolveKeywordEntries('places > europe, Dogs < Animals', existing)).toEqual([
      'places|europe',
      'Animals|Dogs'
    ]);
    expect(resolveKeywordEntries('places > asia', [])).toEqual(['places|asia']);
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

describe('deleting a keyword and keywords without photographs', () => {
  it('moves the keywords under a deleted keyword up one level, as the backend does', () => {
    expect(keywordWithout('Animals|Dogs', 'animals|dogs')).toBeNull();
    expect(keywordWithout('Animals|Dogs|Pugs', 'Animals|Dogs')).toBe('Animals|Pugs');
    expect(keywordWithout('Animals|Dogs', 'Animals')).toBe('Dogs');
    expect(keywordWithout('Cats', 'Dogs')).toBe('Cats');
    expect(
      deleteKeywordTags(
        ['user:Animals|Dogs', 'user:Animals|Dogs|Beagle', 'user:Animals|Beagle', 'color:red'],
        'Animals|Dogs'
      )
    ).toEqual(['user:Animals|Beagle', 'color:red']);
    expect(deleteKeywordTags(['user:Animals', 'user:Animals|Dogs'], 'Animals')).toEqual(['user:Dogs']);
  });

  it('lists a keyword made without photographs, and its parents, with a count of 0', () => {
    const values = [{ value: 'Animals', count: 2 }];
    expect(withDeclaredKeywords(values, ['animals|Birds|Owls', 'Unused'])).toEqual([
      { value: 'Animals', count: 2 },
      { value: 'animals|Birds', count: 0 },
      { value: 'animals|Birds|Owls', count: 0 },
      { value: 'Unused', count: 0 }
    ]);
    const tree = buildKeywordTree(withDeclaredKeywords(values, ['Animals|Birds']));
    expect(tree[0].children.map((child) => [child.name, child.count])).toEqual([['Birds', 0]]);
    expect(tree[0].count).toBe(2);
  });
});
