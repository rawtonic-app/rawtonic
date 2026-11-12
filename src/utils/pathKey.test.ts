// SPDX-License-Identifier: AGPL-3.0-or-later
// WFQ/WPFC (Windows): picks and rejects saved under the library's `\\?\C:\…`
// paths read 0/0 after a restart, when the folder view listed `C:\…`.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { byPathKey, pathKey } from './pathKey';
import { imageFlag } from './culling';
import { showCullingChanges } from './appUndo';
import { useLibraryStore } from '../store/useLibraryStore';
import type { ImageFile } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => []) }));

const verbatim = String.raw`\\?\C:\Users\me\Lib.catalog\Photos\2026\2026-11-11\Canon_R6.CR3`;
const plain = String.raw`C:\Users\me\Lib.catalog\Photos\2026\2026-11-11\Canon_R6.CR3`;

describe('pathKey', () => {
  it('drops the verbatim prefix of drive and UNC paths only', () => {
    expect(pathKey(verbatim)).toBe(plain);
    expect(pathKey(plain)).toBe(plain);
    expect(pathKey(String.raw`\\?\UNC\nas\photos\A.NEF`)).toBe(String.raw`\\nas\photos\A.NEF`);
    expect(pathKey(String.raw`\\?\Volume{1234}\A.NEF`)).toBe(String.raw`\\?\Volume{1234}\A.NEF`);
    expect(pathKey('/Users/me/Lib.catalog/Photos/A.NEF')).toBe('/Users/me/Lib.catalog/Photos/A.NEF');
  });

  it('finds a flag stored under either form by either form', () => {
    for (const stored of [verbatim, plain]) {
      const flags = byPathKey({ [stored]: -1 });
      expect(imageFlag(flags, verbatim)).toBe(-1);
      expect(imageFlag(flags, plain)).toBe(-1);
    }
  });
});

describe('culling receipts', () => {
  beforeEach(() => {
    useLibraryStore.setState({
      imageFlags: {},
      imageRatings: {},
      imageList: [{ path: plain, tags: [] } as unknown as ImageFile]
    });
  });

  it('show a flag and a rating named by the library path on the photo the view lists', () => {
    showCullingChanges([
      { path: verbatim, before: { kind: 'flag', value: 0 }, after: { kind: 'flag', value: 1 } },
      { path: verbatim, before: { kind: 'rating', value: 0 }, after: { kind: 'rating', value: 4 } }
    ]);
    const { imageFlags, imageRatings } = useLibraryStore.getState();
    expect(imageFlag(imageFlags, plain)).toBe(1);
    expect(imageRatings[plain]).toBe(4);
  });
});
