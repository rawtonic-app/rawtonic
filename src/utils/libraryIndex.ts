import type { ImageFile } from '../components/ui/AppProperties';

// Metadata arrives for individual visible tiles. Index each immutable source
// snapshot once instead of scanning every photograph on each thumbnail frame.
const indices = new WeakMap<ImageFile[], Map<string, number>>();

export function libraryImageIndex(files: ImageFile[]): Map<string, number> {
  let index = indices.get(files);
  if (!index) {
    index = new Map(files.map((file, position) => [file.path, position]));
    indices.set(files, index);
  }
  return index;
}
