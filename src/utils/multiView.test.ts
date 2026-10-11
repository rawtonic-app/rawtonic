import { describe, expect, it } from 'vitest';
import {
  CELL_GAP,
  GRID_PADDING,
  MAX_CELLS,
  captionHeight,
  captionLevel,
  captureLine,
  captureValues,
  cellsFor,
  changedCopyableKeys,
  fileName,
  orderedSelection,
  packGrid,
  stepPrimary,
} from './multiView';
import { INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';

describe('packGrid', () => {
  it('puts two landscape photographs side by side in a wide viewer', () => {
    const grid = packGrid(2, 1600, 800, 1.5);
    expect([grid.columns, grid.rows]).toEqual([2, 1]);
    const [a, b] = grid.cells;
    expect(a.cell.x).toBe(GRID_PADDING);
    expect(b.cell.x - (a.cell.x + a.cell.width)).toBeCloseTo(CELL_GAP);
    expect(a.cell.width).toBeCloseTo((1600 - 2 * GRID_PADDING - CELL_GAP) / 2);
  });

  it('stacks two landscape photographs in a tall viewer', () => {
    const grid = packGrid(2, 700, 1200, 1.5);
    expect([grid.columns, grid.rows]).toEqual([1, 2]);
  });

  it('packs four photographs 2 × 2 in a 3:2 viewer and twelve 4 × 3', () => {
    expect(packGrid(4, 1500, 1000, 1.5)).toMatchObject({ columns: 2, rows: 2 });
    expect(packGrid(12, 1500, 1000, 1.5)).toMatchObject({ columns: 4, rows: 3 });
  });

  it('chooses the rows and columns that show the photographs largest', () => {
    // Portrait photographs fill more of a wide viewer in one row.
    expect(packGrid(3, 1500, 700, 2 / 3)).toMatchObject({ columns: 3, rows: 1 });
    // Wide panoramas stack.
    expect(packGrid(3, 1000, 1000, 3)).toMatchObject({ columns: 1, rows: 3 });
  });

  it('never makes more than twelve cells and keeps every cell inside the viewer', () => {
    const grid = packGrid(40, 1200, 800);
    expect(grid.cells).toHaveLength(MAX_CELLS);
    for (const { cell, photo } of grid.cells) {
      expect(cell.x).toBeGreaterThanOrEqual(GRID_PADDING - 1e-6);
      expect(cell.y).toBeGreaterThanOrEqual(GRID_PADDING - 1e-6);
      expect(cell.x + cell.width).toBeLessThanOrEqual(1200 - GRID_PADDING + 1e-6);
      expect(cell.y + cell.height).toBeLessThanOrEqual(800 - GRID_PADDING + 1e-6);
      // The photograph's area is the cell above its caption.
      expect(photo.height).toBeCloseTo(cell.height - captionHeight(captionLevel(cell.width)));
    }
  });

  it('centres a short last row', () => {
    const grid = packGrid(3, 1000, 800, 1.5);
    expect([grid.columns, grid.rows]).toEqual([2, 2]);
    const last = grid.cells[2].cell;
    expect(last.x + last.width / 2).toBeCloseTo(500);
  });

  it('has nothing to lay out without room or photographs', () => {
    expect(packGrid(0, 800, 600).cells).toEqual([]);
    expect(packGrid(3, 0, 600).cells).toEqual([]);
  });
});

describe('captions', () => {
  const exif = { PhotographicSensitivity: '400', ExposureTime: '1/250', FNumber: '2.8', FocalLength: '35' };

  it('grow with the cell width', () => {
    expect(captionLevel(200)).toBe('narrow');
    expect(captionLevel(400)).toBe('medium');
    expect(captionLevel(800)).toBe('wide');
    expect(captionHeight('narrow')).toBeLessThan(captionHeight('medium'));
  });

  it('show one capture value when narrow, ISO/shutter/aperture in a 2-up, focal length too when wide', () => {
    const values = captureValues(exif);
    expect(captureLine('narrow', values)).toBe('1/250 s');
    expect(captureLine('medium', values)).toBe('ISO 400 · 1/250 s · f/2.8');
    expect(captureLine('wide', values)).toBe('ISO 400 · 1/250 s · f/2.8 · 35 mm');
  });

  it('leave out what the file does not record', () => {
    expect(captureLine('wide', captureValues({ FNumber: 'f/4' }))).toBe('f/4');
    expect(captureLine('narrow', captureValues(null))).toBe('');
  });

  it('name virtual copies by their file', () => {
    expect(fileName('/a/b/IMG_1.CR3')).toBe('IMG_1.CR3');
    expect(fileName('/a/b/IMG_1.CR3?vc=2')).toBe('IMG_1.CR3 ⧉');
  });
});

describe('cellsFor', () => {
  const many = Array.from({ length: 15 }, (_, i) => `p${i}`);

  it('shows every photograph up to twelve', () => {
    expect(cellsFor(many.slice(0, 12), 'p0')).toEqual({ shown: many.slice(0, 12), more: 0 });
  });

  it('keeps the last cell for "+N more" and the primary always visible', () => {
    expect(cellsFor(many, 'p3')).toEqual({ shown: many.slice(0, 11), more: 4 });
    const later = cellsFor(many, 'p13');
    expect(later.shown).toHaveLength(11);
    expect(later.shown).toContain('p13');
    expect(later.more).toBe(4);
  });
});

describe('primary navigation', () => {
  const selection = ['a', 'b', 'c'];

  it('steps through the selection and wraps', () => {
    expect(stepPrimary(selection, 'a', 1)).toBe('b');
    expect(stepPrimary(selection, 'c', 1)).toBe('a');
    expect(stepPrimary(selection, 'a', -1)).toBe('c');
    expect(stepPrimary(selection, 'x', 1)).toBe('a');
    expect(stepPrimary([], 'a', 1)).toBeNull();
  });

  it('orders the selection as the browser lists it', () => {
    expect(orderedSelection(['a', 'b', 'c', 'd'], ['d', 'b'])).toEqual(['b', 'd']);
    expect(orderedSelection(['a'], ['z', 'a'])).toEqual(['a', 'z']);
  });
});

describe('changedCopyableKeys', () => {
  it('takes the copyable keys an edit changed, with their new values', () => {
    const before = { ...INITIAL_ADJUSTMENTS } as Adjustments;
    const after = { ...before, exposure: 0.7, contrast: 12, crop: { x: 1, y: 2, width: 3, height: 4 } } as Adjustments;
    const changed = changedCopyableKeys(before, after);
    expect(changed).toMatchObject({ exposure: 0.7, contrast: 12 });
    // The crop belongs to each photograph only if it is copyable.
    expect(Object.keys(changed).every((key) => key !== 'aiPatches' && key !== 'spots')).toBe(true);
  });

  it('finds nothing when only per-photograph data changed', () => {
    const before = { ...INITIAL_ADJUSTMENTS } as Adjustments;
    const after = { ...before, spots: [{ id: 's' }] } as unknown as Adjustments;
    expect(changedCopyableKeys(before, after)).toEqual({});
  });
});
