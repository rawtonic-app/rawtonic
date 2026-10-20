import { describe, expect, it, vi } from 'vitest';
import { INITIAL_ADJUSTMENTS } from './adjustments';
import cacheUtils from '../../crates/engine/src/cache_utils.rs?raw';
import { GEOMETRY_KEYS } from './geometryKeys';
import { geometryKey } from '../hooks/useMaskThumbnails';
import { getTransformAdjustments } from '../hooks/useAiMasking';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

describe('geometry keys', () => {
  it('are the engine’s GEOMETRY_KEYS', () => {
    const engine = [...(cacheUtils.match(/GEOMETRY_KEYS: &\[&str\] = &\[([\s\S]*?)\];/)?.[1] ?? '').matchAll(/"([^"]+)"/g)].map(
      (match) => match[1],
    );
    expect(engine.length).toBeGreaterThan(20);
    expect([...GEOMETRY_KEYS]).toEqual(engine);
  });
});

describe('mask thumbnails and AI masks', () => {
  it('follow every geometry key, fringeVersion included', () => {
    const base = { ...INITIAL_ADJUSTMENTS, fringeVersion: 1 };
    expect(geometryKey({ ...base, fringeVersion: 0 })).not.toBe(geometryKey(base));
    expect(getTransformAdjustments(base)).toMatchObject({ fringeVersion: 1 });
    expect(Object.keys(getTransformAdjustments(base)).sort()).toEqual([...GEOMETRY_KEYS].sort());
  });
});
