import { pathKey } from './pathKey';
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

const aliases = new WeakMap<ImageFile[], Map<string, string>>();

export function libraryImagePath(files: ImageFile[], emittedPath: string): string {
  if (libraryImageIndex(files).has(emittedPath)) return emittedPath;
  let index = aliases.get(files);
  if (!index) {
    index = new Map();
    for (const file of files) {
      const key = pathKey(file.path);
      if (!index.has(key)) index.set(key, file.path);
    }
    aliases.set(files, index);
  }
  return index.get(pathKey(emittedPath)) ?? emittedPath;
}
