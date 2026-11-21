import { describe, expect, it } from 'vitest';
import type { Adjustments } from './adjustments';
import { selectionsToDetectAgain, settleMigrated, withDetectedAgain } from './migratedSelections';

const recipe = () =>
  ({
    masks: [
      {
        id: 'sky',
        subMasks: [
          {
            id: 's',
            type: 'ai-sky',
            parameters: {
              maskDataBase64: 'data:thumb',
              bitmapFrame: { width: 288, height: 192 },
              detectAgain: true,
              grow: 5
            }
          },
          { id: 'b', type: 'brush', parameters: { lines: [] } }
        ]
      },
      {
        id: 'people',
        subMasks: [
          {
            id: 'p',
            type: 'ai-people',
            parameters: { maskDataBase64: 'data:thumb', detectAgain: true, parts: ['hair'], person: null }
          },
          { id: 'o', type: 'ai-subject', parameters: { maskDataBase64: 'data:thumb', bitmapFrame: {} } }
        ]
      }
    ]
  }) as unknown as Adjustments;

const params = (a: Adjustments, layer: number, sub: number) =>
  a.masks[layer].subMasks[sub].parameters as Record<string, unknown>;

describe('migrated selections', () => {
  it('lists the selections waiting to be computed again, not Object selections', () => {
    expect(selectionsToDetectAgain(recipe()).map((w) => w.sub.id)).toEqual(['s', 'p']);
  });

  it('puts the full-frame selections in, keeping refinements, in the layers asked', () => {
    const results = new Map([
      ['s', { asked: params(recipe(), 0, 0), result: { maskDataBase64: 'data:full', rotation: 0 } }],
      ['p', { asked: params(recipe(), 1, 0), result: { maskDataBase64: 'data:people' } }]
    ]);
    const { adjustments, layers } = withDetectedAgain(recipe(), results);
    expect(layers).toBe(2);
    expect(params(adjustments, 0, 0)).toEqual({ maskDataBase64: 'data:full', rotation: 0, grow: 5 });
    expect(params(adjustments, 1, 0).maskDataBase64).toBe('data:people');
    expect(params(adjustments, 1, 1).maskDataBase64).toBe('data:thumb');
    expect(adjustments.masks[0].subMasks[0].id).toBe('s');
    expect(selectionsToDetectAgain(adjustments)).toEqual([]);
  });

  it('leaves a People selection whose parts changed meanwhile', () => {
    const now = recipe();
    (now.masks[1].subMasks[0].parameters as Record<string, unknown>).parts = ['faceSkin'];
    const results = new Map([['p', { asked: params(recipe(), 1, 0), result: { maskDataBase64: 'data:people' } }]]);
    expect(withDetectedAgain(now, results).layers).toBe(0);
  });

  it('settles the earlier steps: the stretched bitmap stays without the mark', () => {
    const settled = settleMigrated(recipe(), new Set(['s']));
    expect(params(settled, 0, 0)).toEqual({
      maskDataBase64: 'data:thumb',
      bitmapFrame: { width: 288, height: 192 },
      grow: 5
    });
    expect(params(settled, 1, 0).detectAgain).toBe(true);
    const untouched = { masks: [] } as unknown as Adjustments;
    expect(settleMigrated(untouched, new Set(['s']))).toBe(untouched);
  });
});
