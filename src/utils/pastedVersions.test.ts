import { describe, expect, it } from 'vitest';
import cases from '../../crates/engine/tests/data/pasted_versions.json';
import { expressPastedSettings, structureAmountV2, structureValueV2 } from './pastedVersions';
import type { Adjustments } from './adjustments';

/** `got` equals `want`, numbers to 1e-6 (as the engine's test compares). */
function expectClose(got: unknown, want: unknown, name: string) {
  if (typeof want === 'number') {
    expect(typeof got, name).toBe('number');
    expect(Math.abs((got as number) - want), `${name}: ${got} != ${want}`).toBeLessThanOrEqual(1e-6);
  } else if (Array.isArray(want)) {
    expect(Array.isArray(got), name).toBe(true);
    expect((got as unknown[]).length, name).toBe(want.length);
    want.forEach((w, i) => expectClose((got as unknown[])[i], w, `${name}[${i}]`));
  } else if (want && typeof want === 'object') {
    expect(Object.keys(got as object).sort(), `${name}: keys`).toEqual(Object.keys(want).sort());
    for (const [k, w] of Object.entries(want)) expectClose((got as Record<string, unknown>)[k], w, `${name}.${k}`);
  } else {
    expect(got, name).toEqual(want);
  }
}

describe('expressPastedSettings', () => {
  it.each(cases.map((c) => [c.name, c] as const))('%s', (name, c) => {
    const destination = c.destination as Partial<Adjustments>;
    const got = expressPastedSettings(c.pasted as Partial<Adjustments>, {
      recipe: (c as { isNew?: boolean }).isNew ? null : destination,
      isRaw: Boolean((c as { isRaw?: boolean }).isRaw),
      replaceLayers: Boolean((c as { replaceLayers?: boolean }).replaceLayers)
    });
    expectClose(got, c.expected, name);
  });

  it('Structure between versions 1 and 2 keeps the f32 amount', () => {
    for (let i = -1000; i <= 1000; i++) {
      const amount = structureAmountV2(i / 10);
      expect(Math.fround(structureAmountV2(structureValueV2(amount)))).toBe(Math.fround(amount));
    }
  });
});
