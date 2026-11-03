import { describe, expect, it } from 'vitest';
import vectors from '../../../crates/metadata/src/photo_ref_vectors.json';
import {
  copyOf,
  formatPhotoRef,
  isCopy,
  isCopyOf,
  parsePhotoRef,
  sourceFileName,
  sourceOf,
  withSource
} from './photoRef';

describe('photoRef', () => {
  it('parses and formats every stored form as the Rust codec does', () => {
    expect(vectors.length).toBeGreaterThanOrEqual(15);
    for (const v of vectors) {
      const ref = parsePhotoRef(v.text);
      expect(ref, v.text).toEqual({ source: v.source, copy: v.copy });
      expect(formatPhotoRef(ref)).toBe(v.text);
      expect(sourceOf(v.text)).toBe(v.source);
      expect(copyOf(v.text)).toBe(v.copy);
      expect(isCopy(v.text)).toBe(v.copy !== null);
      if (v.copy !== null) {
        expect(isCopyOf(v.text, v.source)).toBe(true);
        expect(withSource(v.text, '/q/b.raf')).toBe(`/q/b.raf?vc=${v.copy}`);
      } else {
        expect(withSource(v.text, '/q/b.raf')).toBe('/q/b.raf');
      }
    }
  });

  it('a copy of is only a copy of that exact source', () => {
    expect(isCopyOf('/p/a.raf?vc=1', '/p/a.raf')).toBe(true);
    expect(isCopyOf('/p/a.raf', '/p/a.raf')).toBe(false);
    expect(isCopyOf('/p/a.raf.jpg?vc=1', '/p/a.raf')).toBe(false);
    expect(isCopyOf('/p/a.raf?vc=b.raf?vc=abc123', '/p/a.raf')).toBe(false);
    expect(isCopyOf('/p/a.raf?vc=b.raf?vc=abc123', '/p/a.raf?vc=b.raf')).toBe(true);
  });

  it('names the source file on either separator', () => {
    expect(sourceFileName('/p/a.raf?vc=0f1e2d')).toBe('a.raf');
    expect(sourceFileName('C:\\Photos\\a.NEF?vc=1a2b3c')).toBe('a.NEF');
    expect(sourceFileName('a.raf')).toBe('a.raf');
  });
});
