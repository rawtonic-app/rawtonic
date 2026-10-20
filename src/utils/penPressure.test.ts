import { describe, expect, it } from 'vitest';
import { pointPressure, pressureKeys, pressureMapping, withPressure } from './penPressure';

const both = { size: true, flow: true };

describe('pen pressure', () => {
  it('stores pressure only for a pen with a mapping on', () => {
    expect(pointPressure({ pointerType: 'mouse', pressure: 0.5 }, both)).toBeUndefined();
    expect(pointPressure({ pointerType: 'touch', pressure: 0.3 }, both)).toBeUndefined();
    expect(pointPressure({ pointerType: 'pen', pressure: 0.3 }, { size: false, flow: false })).toBeUndefined();
    expect(pointPressure({ pointerType: 'pen', pressure: 0.12345 }, both)).toBe(0.123);
    expect(pointPressure({ pointerType: 'pen', pressure: 1.4 }, both)).toBe(1);
    expect(pointPressure(null, both)).toBeUndefined();
  });

  it('leaves mouse strokes without new keys', () => {
    const mouse = [withPressure({ x: 1, y: 2 }, undefined)];
    expect(mouse[0]).toEqual({ x: 1, y: 2 });
    expect(JSON.stringify(mouse[0])).toBe('{"x":1,"y":2}');
    expect(pressureKeys(mouse, both)).toEqual({});
    const pen = [withPressure({ x: 1, y: 2 }, 0.4)];
    expect(pressureKeys(pen, pressureMapping(null))).toEqual({ pressureSize: true });
    expect(pressureKeys(pen, both)).toEqual({ pressureSize: true, pressureFlow: true });
  });
});
