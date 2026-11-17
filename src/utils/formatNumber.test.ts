import { describe, expect, it } from 'vitest';
import { formatNumber } from './formatNumber';

describe('formatNumber', () => {
  it('normalizes negative zero at the displayed precision', () => {
    for (const value of [-0, -0.0001, 0, 0.0001]) {
      expect(formatNumber(value, 2)).toBe('0.00');
      expect(formatNumber(value, 2, { trim: true })).toBe('0');
      expect(formatNumber(value, 2, { signed: true })).toBe('+0.00');
    }
  });
  it('keeps nonzero signs, precision and rounding boundaries', () => {
    expect(formatNumber(-0.006, 2)).toBe('-0.01');
    expect(formatNumber(-0.006, 2, { signed: true })).toBe('−0.01');
    expect(formatNumber(1.6, 2, { trim: true })).toBe('1.6');
    expect(formatNumber(1.6, 2)).toBe('1.60');
    expect(formatNumber(-0.4)).toBe('0');
    expect(formatNumber(-0.6)).toBe('-1');
    expect(formatNumber(-0.00000001, 6)).toBe('0.000000');
  });
});
