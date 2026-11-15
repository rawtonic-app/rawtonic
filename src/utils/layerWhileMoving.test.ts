import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { compositeWhileMoving } from './layerWhileMoving';

describe('compositeWhileMoving', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('composites while moving and drops the hint once the element rests', () => {
    const element = document.createElement('div');
    compositeWhileMoving(element, 200);
    expect(element.style.willChange).toBe('transform');
    vi.advanceTimersByTime(150);
    compositeWhileMoving(element, 200);
    vi.advanceTimersByTime(150);
    // Still moving: 150 ms since the last move.
    expect(element.style.willChange).toBe('transform');
    vi.advanceTimersByTime(50);
    expect(element.style.willChange).toBe('');
  });
});
