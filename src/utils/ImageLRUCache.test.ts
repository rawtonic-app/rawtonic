import { afterEach, expect, it, vi } from 'vitest';
import { globalImageCache } from './ImageLRUCache';
import { INITIAL_ADJUSTMENTS } from './adjustments';
import type { SelectedImage } from '../components/ui/AppProperties';

afterEach(() => {
  globalImageCache.clear();
  vi.unstubAllGlobals();
});

it('does not retain full camera JPEG placeholders in the twenty-image editor cache', () => {
  const revoke = vi.fn();
  vi.stubGlobal('URL', { revokeObjectURL: revoke });
  const entry = (url: string) => ({
    adjustments: INITIAL_ADJUSTMENTS, histogram: null, waveform: null,
    finalPreviewUrl: null, uncroppedPreviewUrl: null,
    selectedImage: { thumbnailUrl: url, thumbnailSource: 'embedded' } as SelectedImage,
    originalSize: { width: 1, height: 1 }, previewSize: { width: 1, height: 1 },
  });
  const activeEntry = entry('blob:first');
  globalImageCache.set('first', activeEntry);
  expect(globalImageCache.isProtected('blob:first')).toBe(false);
  expect(globalImageCache.get('first')?.selectedImage).toEqual({ thumbnailUrl: '', thumbnailSource: undefined });
  expect(activeEntry.selectedImage.thumbnailUrl).toBe('blob:first');
  globalImageCache.clear();
  // The active loader owns the temporary blob until the selection changes.
  expect(revoke).not.toHaveBeenCalled();
});
