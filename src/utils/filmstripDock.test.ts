import { describe, expect, it } from 'vitest';
import { DOCK_SIZE, DOCK_WIDTH, dockExtent, dockSideAt, readFilmstripDock, wheelThumbnailSize } from './filmstripDock';

describe('readFilmstripDock', () => {
  it('shows the strip under the photograph unless a side was chosen', () => {
    expect(readFilmstripDock(undefined).side).toBe('bottom');
    expect(readFilmstripDock({ side: 'left', names: true, size: 100, width: 260 })).toEqual({
      side: 'left',
      names: true,
      size: 100,
      width: 260
    });
  });
  it('replaces unreadable and out-of-range values', () => {
    expect(readFilmstripDock({ side: 'top', names: 'yes', size: 'big', width: NaN })).toEqual(readFilmstripDock({}));
    const wide = readFilmstripDock({ size: 5000, width: 1 });
    expect(wide.size).toBe(DOCK_SIZE.max);
    expect(wide.width).toBe(DOCK_WIDTH.min);
  });
});

describe('wheelThumbnailSize', () => {
  it('enlarges on a turn up, shrinks on a turn down, within the limits', () => {
    expect(wheelThumbnailSize(88, -100)).toBe(88 + DOCK_SIZE.step);
    expect(wheelThumbnailSize(88, 100)).toBe(88 - DOCK_SIZE.step);
    expect(wheelThumbnailSize(DOCK_SIZE.max, -100)).toBe(DOCK_SIZE.max);
    expect(wheelThumbnailSize(DOCK_SIZE.min, 100)).toBe(DOCK_SIZE.min);
  });
  it('moves at least a step for a small trackpad delta and none for zero', () => {
    expect(wheelThumbnailSize(88, -3)).toBe(88 + DOCK_SIZE.step);
    expect(wheelThumbnailSize(88, 0)).toBe(88);
  });
});

describe('dockSideAt', () => {
  const area = { left: 0, top: 0, width: 1000, height: 500 };
  it('takes the outer quarter of a side and the lower fifth', () => {
    expect(dockSideAt(100, 200, area)).toBe('left');
    expect(dockSideAt(900, 200, area)).toBe('right');
    expect(dockSideAt(500, 450, area)).toBe('bottom');
    expect(dockSideAt(500, 200, area)).toBeNull();
    expect(dockSideAt(-5, 200, area)).toBeNull();
  });
});

describe('dockExtent', () => {
  it('adds room for a file name', () => {
    expect(dockExtent(100, true)).toBeGreaterThan(dockExtent(100, false));
  });
});
