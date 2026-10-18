import type { SelectedImage } from '../components/ui/AppProperties';

/**
 * The photo whose Auto Mask source the backend should build ahead of the first
 * stroke (`prewarm_mask_source`), or null. Only the full decode has that
 * source: a photo shown from its preview proxy waits until the decode is in.
 */
export function autoMaskPrewarmPath(
  isMasking: boolean,
  autoMask: boolean | undefined,
  image: Pick<SelectedImage, 'path' | 'isReady' | 'isDecoded'> | null | undefined,
): string | null {
  if (!isMasking || !autoMask || !image?.isReady || !image.isDecoded) return null;
  return image.path || null;
}
