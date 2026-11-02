import { afterEach, describe, expect, it } from 'vitest';
import {
  DEFAULT_PROOF_DRAFT,
  pageSize,
  readProofDraft,
  sheetFromDraft,
  sheetPhotos,
  sheetSharpening,
  writeProofDraft,
} from './proofSheet';

afterEach(() => localStorage.clear());

describe('proof sheet form', () => {
  it('turns the paper for landscape and keeps custom sizes in range', () => {
    expect(pageSize(DEFAULT_PROOF_DRAFT)).toEqual([210, 297]);
    expect(pageSize({ ...DEFAULT_PROOF_DRAFT, landscape: true })).toEqual([297, 210]);
    expect(pageSize({ ...DEFAULT_PROOF_DRAFT, paper: 'letter' })).toEqual([215.9, 279.4]);
    expect(pageSize({ ...DEFAULT_PROOF_DRAFT, paper: 'custom', customWidthMm: 5, customHeightMm: Number.NaN })).toEqual([
      20, 297,
    ]);
  });

  it('sends the backend a layout it can lay out', () => {
    const sheet = sheetFromDraft({ ...DEFAULT_PROOF_DRAFT, columns: 4.4, rows: 0, caption: '  ', header: '{job}' });
    expect(sheet).toMatchObject({
      pageWidthMm: 210,
      pageHeightMm: 297,
      marginsMm: [12, 12, 12, 12],
      columns: 4,
      rows: 1,
      caption: null,
      header: '{job}',
      ppi: 300,
      pdf: true,
      jpeg: false,
    });
    expect(sheetSharpening({ ...DEFAULT_PROOF_DRAFT, sharpen: false })).toBeNull();
    // The export's print sharpening for 300 ppi seen from 30 cm.
    expect(sheetSharpening(DEFAULT_PROOF_DRAFT)?.radius).toBeCloseTo(1.03, 2);
  });

  it('is remembered per library and ignores what it does not know', () => {
    writeProofDraft('/jobs/A', { ...DEFAULT_PROOF_DRAFT, columns: 6, folder: '/out' });
    expect(readProofDraft('/jobs/A')).toMatchObject({ columns: 6, folder: '/out' });
    expect(readProofDraft('/jobs/B').columns).toBe(4);
    localStorage.setItem('rawtonic.proofSheet.form:/jobs/C', JSON.stringify({ columns: 'many', paper: 'b5', extra: 1 }));
    const stale = readProofDraft('/jobs/C');
    expect(stale.columns).toBe(4);
    expect(stale.paper).toBe('a4');
    expect('extra' in stale).toBe(false);
  });

  it('takes the selection in the order the grid shows it, else every photo shown', () => {
    const photo = (n: number) => `/p/${n}.NEF`;
    const shown = [1, 2, 3, 4].map(photo);
    expect(sheetPhotos(shown, [])).toEqual(shown);
    expect(sheetPhotos(shown, [photo(4), photo(2)])).toEqual([photo(2), photo(4)]);
    expect(sheetPhotos(shown, [photo(9), photo(3)])).toEqual([photo(3), photo(9)]);
  });
});
