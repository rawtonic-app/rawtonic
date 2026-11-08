import { describe, expect, it } from 'vitest';
import { applyReviewKey, cardInsertAction, cardToEject, onCard, reviewKey, withRatings } from './ingestReview';
import type { ImportMetadata } from './IngestMetadata';

describe('review keys', () => {
  it('P, X and 0–5 only, without modifier keys', () => {
    expect(reviewKey({ key: 'p' })).toEqual({ kind: 'include' });
    expect(reviewKey({ key: 'X' })).toEqual({ kind: 'exclude' });
    expect(reviewKey({ key: '4' })).toEqual({ kind: 'rate', rating: 4 });
    expect(reviewKey({ key: '0' })).toEqual({ kind: 'rate', rating: 0 });
    expect(reviewKey({ key: '6' })).toBeNull();
    expect(reviewKey({ key: 'p', metaKey: true })).toBeNull();
    expect(reviewKey({ key: 'z' })).toBeNull();
  });

  it('applies them without changing what it does not touch', () => {
    const selected = new Set(['a']);
    const ratings = { a: 2 };
    const rated = applyReviewKey({ kind: 'rate', rating: 5 }, 'b', selected, ratings);
    expect(rated.selected).toBe(selected);
    expect(rated.ratings).toEqual({ a: 2, b: 5 });
    expect(applyReviewKey({ kind: 'rate', rating: 0 }, 'a', selected, ratings).ratings).toEqual({});
    expect([...applyReviewKey({ kind: 'include' }, 'b', selected, ratings).selected]).toEqual(['a', 'b']);
    expect([...applyReviewKey({ kind: 'exclude' }, 'a', selected, ratings).selected]).toEqual([]);
  });
});

describe('ratings in the import', () => {
  const preset = { creator: 'Me', keywords: [] } as unknown as ImportMetadata;
  it('sends only the imported photographs’ ratings, with the preset or alone', () => {
    const ratings = { '/c/A.NEF': 3, '/c/B.NEF': 5 };
    // Exactly what `session_ingest` gets as `metadata` (Rust deserializes it).
    expect(withRatings(null, ratings, ['/c/A.NEF'])).toEqual({ ratings: { '/c/A.NEF': 3 } });
    expect(withRatings(preset, ratings, ['/c/A.NEF', '/c/C.NEF'])).toEqual({ ...preset, ratings: { '/c/A.NEF': 3 } });
    expect(withRatings(preset, ratings, ['/c/C.NEF'])).toBe(preset);
    expect(withRatings(null, {}, ['/c/A.NEF'])).toBeNull();
  });
});

describe('memory cards', () => {
  it('opens the import only when nothing else is going on; waits for an export', () => {
    const idle = { enabled: true, importing: false, tethering: false, exporting: false, dialogOpen: false };
    expect(cardInsertAction(idle)).toBe('open');
    expect(cardInsertAction({ ...idle, enabled: false })).toBe('ignore');
    expect(cardInsertAction({ ...idle, importing: true })).toBe('ignore');
    expect(cardInsertAction({ ...idle, tethering: true })).toBe('ignore');
    expect(cardInsertAction({ ...idle, dialogOpen: true })).toBe('ignore');
    expect(cardInsertAction({ ...idle, exporting: true })).toBe('wait');
  });

  it('knows which paths are on a card, by whole path components', () => {
    expect(onCard('/Volumes/CARD/DCIM/A.NEF', '/Volumes/CARD')).toBe(true);
    expect(onCard('/Volumes/CARD2/DCIM/A.NEF', '/Volumes/CARD')).toBe(false);
    expect(onCard('E:\\DCIM\\A.NEF', 'E:\\')).toBe(true);
  });

  it('ejects only after every photograph came off the card without an error', () => {
    const files = ['/Volumes/CARD/DCIM/A.NEF', '/Volumes/CARD/DCIM/B.NEF'];
    const ok = files.map((source) => ({ source, error: null }));
    expect(cardToEject('/Volumes/CARD', files, ok)).toBe('/Volumes/CARD');
    expect(cardToEject(null, files, ok)).toBeNull();
    expect(cardToEject('/Volumes/CARD', files, ok.slice(0, 1))).toBeNull();
    expect(cardToEject('/Volumes/CARD', files, [ok[0], { source: files[1], error: 'checksum' }])).toBeNull();
    expect(cardToEject('/Volumes/CARD', [...files, '/Users/me/C.NEF'], ok)).toBeNull();
  });
});
