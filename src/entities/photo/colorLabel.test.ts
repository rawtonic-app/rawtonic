import { describe, expect, it } from 'vitest';
import { COLOR_LABEL_NAMES, isColorLabelName } from './colorLabel';

describe('colorLabel', () => {
  it('names the five labels of the Rust ColorLabel, lower case, in order', () => {
    expect(COLOR_LABEL_NAMES).toEqual(['red', 'yellow', 'green', 'blue', 'purple']);
    expect(isColorLabelName('blue')).toBe(true);
    expect(isColorLabelName('Blue')).toBe(false);
    expect(isColorLabelName('orange')).toBe(false);
  });
});
