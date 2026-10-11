import { beforeEach, describe, expect, it, vi } from 'vitest';
import { loadLibraryImages } from './libraryListing';
import type { ImageFile } from '../components/ui/AppProperties';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
beforeEach(() => invoke.mockReset());
const files = (count: number) => Array.from({ length: count }, (_, i) => ({
  path: `/qa/${i}.dng`, rating: i % 6, tags: ['user:fixture'], is_raw: true,
  modified: i, is_edited: i % 2 === 0, is_virtual_copy: false,
  is_cloud_placeholder: false, group_id: `group-${i}`, exif: null,
})) satisfies ImageFile[];

describe('bounded library listing transfer', () => {
  it('publishes one complete snapshot with every record, field and original order', async () => {
    const expected = files(1025);
    invoke.mockResolvedValueOnce({ listing_id: 'one', offset: 0, total: expected.length, files: expected.slice(0, 512) })
      .mockResolvedValueOnce({ listing_id: 'one', offset: 512, total: expected.length, files: expected.slice(512, 1024) })
      .mockResolvedValueOnce({ listing_id: null, offset: 1024, total: expected.length, files: expected.slice(1024) });
    await expect(loadLibraryImages('/qa', true)).resolves.toEqual(expected);
    expect(invoke.mock.calls).toEqual([
      ['begin_image_listing', { path: '/qa', recursive: true }],
      ['next_image_listing_page', { listingId: 'one', offset: 512 }],
      ['next_image_listing_page', { listingId: 'one', offset: 1024 }],
    ]);
  });

  it('does not start a cancelled request', async () => {
    const abort = new AbortController();
    abort.abort();
    await expect(loadLibraryImages('/qa', false, abort.signal)).rejects.toMatchObject({ name: 'AbortError' });
    expect(invoke).not.toHaveBeenCalled();
  });

  it('releases a late scan response when navigation changed before its first page', async () => {
    const abort = new AbortController();
    let finish!: (value: unknown) => void;
    invoke.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; })).mockResolvedValue(undefined);
    const result = loadLibraryImages('/old', false, abort.signal);
    const rejected = expect(result).rejects.toMatchObject({ name: 'AbortError' });
    abort.abort();
    finish({ listing_id: 'old', offset: 0, total: 700, files: files(512) });
    await rejected;
    expect(invoke.mock.calls.at(-1)).toEqual(['cancel_image_listing', { listingId: 'old' }]);
    expect(invoke.mock.calls.some(([command]) => command === 'next_image_listing_page')).toBe(false);
  });

  it('releases the snapshot when a later page fails, without returning partial data', async () => {
    invoke.mockResolvedValueOnce({ listing_id: 'one', offset: 0, total: 700, files: files(512) })
      .mockRejectedValueOnce(new Error('IPC failed')).mockResolvedValue(undefined);
    await expect(loadLibraryImages('/qa', false)).rejects.toThrow('IPC failed');
    expect(invoke.mock.calls.at(-1)).toEqual(['cancel_image_listing', { listingId: 'one' }]);
  });

  it('rejects out-of-order or incomplete responses instead of silently losing photographs', async () => {
    invoke.mockResolvedValueOnce({ listing_id: 'one', offset: 0, total: 700, files: files(512) })
      .mockResolvedValueOnce({ listing_id: null, offset: 500, total: 700, files: files(188) })
      .mockResolvedValue(undefined);
    await expect(loadLibraryImages('/qa', false)).rejects.toThrow('Invalid library listing page');
    expect(invoke.mock.calls.at(-1)).toEqual(['cancel_image_listing', { listingId: 'one' }]);
    invoke.mockReset().mockResolvedValue({ listing_id: null, offset: 0, total: 2, files: files(1) });
    await expect(loadLibraryImages('/qa', false)).rejects.toThrow('Incomplete library listing');
  });
});
