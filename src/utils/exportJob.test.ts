import { beforeEach, describe, expect, it } from 'vitest';
import { readJobName, writeJobName } from './exportJob';

beforeEach(() => localStorage.clear());

describe('export job name', () => {
  it("defaults to the library's name, without the catalog folder's extension", () => {
    // A catalog named "Wedding" lives in "Wedding.catalog"; {job} on import is "Wedding".
    expect(readJobName('/Users/me/Shoots/Wedding.catalog')).toBe('Wedding');
    expect(readJobName('/Users/me/Shoots/Wedding.catalog/')).toBe('Wedding');
    expect(readJobName('/Users/me/Shoots/Smith job')).toBe('Smith job');
  });

  it('keeps a name the photographer typed', () => {
    writeJobName('/Users/me/Shoots/Wedding.catalog', 'Smith & Jones');
    expect(readJobName('/Users/me/Shoots/Wedding.catalog')).toBe('Smith & Jones');
  });
});
