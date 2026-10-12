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
  sameZoom,
  sharedZoomOf,
  stepPrimary,
  transformOfZoom,
  zoomCell,
  panCell,
  cycleCellZoom,
  fitScaleOf,
  MAX_CELL_ZOOM,
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

describe('reference photograph', () => {
  const many = Array.from({ length: 15 }, (_, i) => `p${i}`);

  it('comes first and is not counted twice', () => {
    expect(cellsFor(['a', 'b'], 'a', 'r')).toEqual({ shown: ['r', 'a', 'b'], more: 0 });
    expect(cellsFor(['a', 'r', 'b'], 'a', 'r')).toEqual({ shown: ['r', 'a', 'b'], more: 0 });
  });

  it('takes one of the twelve cells; the primary stays visible', () => {
    const cells = cellsFor(many, 'p14', 'ref');
    expect(cells.shown).toHaveLength(11);
    expect(cells.shown[0]).toBe('ref');
    expect(cells.shown).toContain('p14');
    expect(cells.more).toBe(5);
  });
});

describe('sharedZoomOf', () => {
  // A 6000 × 4000 photograph fitted to 600 × 400 CSS px in a 600 × 500
  // container (offset 50 px down), at 2× device pixels.
  const fit = { width: 600, height: 400, offsetX: 0, offsetY: 50, containerWidth: 600, containerHeight: 500 };
  const shown = { width: 6000, height: 4000 };

  it('is nothing at fit', () => {
    expect(sharedZoomOf({ scale: 1, positionX: 0, positionY: 0 }, fit, shown, 2)).toBeNull();
  });

  it('gives device px per photo px and the photograph point at the centre', () => {
    // Scale 5: 3000 CSS px wide = 6000 device px for 6000 photo px → 1:1.
    // The container centre shows CSS (300, 250) of the fit image (its
    // centre): position = centre − point × scale.
    const zoom = sharedZoomOf({ scale: 5, positionX: 300 - 300 * 5, positionY: 250 - 250 * 5 }, fit, shown, 2);
    expect(zoom?.scale).toBeCloseTo(1);
    expect(zoom?.center[0]).toBeCloseTo(0.5);
    expect(zoom?.center[1]).toBeCloseTo(0.5);
    // Panned to the left edge of the photograph.
    const left = sharedZoomOf({ scale: 5, positionX: 300, positionY: 250 - 250 * 5 }, fit, shown, 2);
    expect(left?.center[0]).toBeCloseTo(0);
  });

  it('compares zooms with a tolerance', () => {
    const a = { scale: 1, center: [0.5, 0.5] as [number, number] };
    expect(sameZoom(a, { scale: 1.00001, center: [0.50001, 0.5] })).toBe(true);
    expect(sameZoom(a, { scale: 1.1, center: [0.5, 0.5] })).toBe(false);
    expect(sameZoom(null, null)).toBe(true);
    expect(sameZoom(a, null)).toBe(false);
  });
});

describe('per-cell zoom (zoom lock off)', () => {
  // A 6000 × 4000 photograph in a 600 × 400 CSS px cell at 2× device px:
  // fit is 0.2 device px per photograph px.
  const area = { width: 600, height: 400 };
  const shown = { width: 6000, height: 4000 };

  it('knows the fit of a cell', () => {
    expect(fitScaleOf(area, shown, 2)).toBeCloseTo(0.2);
    expect(fitScaleOf({ width: 600, height: 200 }, shown, 2)).toBeCloseTo(0.1);
  });

  it('zooms around the pointer, keeping the photograph point under it', () => {
    // 100 CSS px right of the centre at fit: photograph x 3000 + 100 × 2 / 0.2 = 4000.
    const zoom = zoomCell(null, 5, [100, 0], area, shown, 2)!;
    expect(zoom.scale).toBeCloseTo(1);
    const under = zoom.center[0] * 6000 + (100 * 2) / zoom.scale;
    expect(under).toBeCloseTo(4000);
    expect(zoom.center[1]).toBeCloseTo(0.5);
  });

  it('returns to fit at or below fit and stops at 200 %', () => {
    const zoom = zoomCell(null, 5, [0, 0], area, shown, 2)!;
    expect(zoomCell(zoom, 1 / 5, [0, 0], area, shown, 2)).toBeNull();
    expect(zoomCell(zoom, 1 / 10, [0, 0], area, shown, 2)).toBeNull();
    expect(zoomCell(zoom, 100, [0, 0], area, shown, 2)!.scale).toBe(MAX_CELL_ZOOM);
  });

  it('keeps the view on the photograph near its edges', () => {
    // Zoomed at the far left: the cell shows photograph x 0 … 1200 (600 CSS px × 2 / 1).
    const zoom = zoomCell(null, 5, [-300, 0], area, shown, 2)!;
    expect(zoom.center[0] * 6000).toBeCloseTo(600);
    const panned = panCell(zoom, 500, 0, area, shown, 2);
    expect(panned.center[0] * 6000).toBeCloseTo(600);
    const back = panCell(zoom, -100, -50, area, shown, 2);
    expect(back.center[0] * 6000).toBeCloseTo(800);
    expect(back.center[1] * 4000).toBeCloseTo(2100);
  });

  it('cycles fit → 100 % → fit with Space', () => {
    const zoom = cycleCellZoom(null, [0, 0], area, shown, 2)!;
    expect(zoom.scale).toBeCloseTo(1);
    expect(cycleCellZoom(zoom, [0, 0], area, shown, 2)).toBeNull();
  });

  it("gives the editor the transform that shows a cell's zoom", () => {
    const fit = { width: 600, height: 400, offsetX: 0, offsetY: 50, containerWidth: 600, containerHeight: 500 };
    const zoom = { scale: 1, center: [0.25, 0.75] as [number, number] };
    const transform = transformOfZoom(zoom, fit, shown, 2)!;
    expect(transform.scale).toBeCloseTo(5);
    const back = sharedZoomOf(transform, fit, shown, 2)!;
    expect(back.scale).toBeCloseTo(1);
    expect(back.center[0]).toBeCloseTo(0.25);
    expect(back.center[1]).toBeCloseTo(0.75);
  });
});
