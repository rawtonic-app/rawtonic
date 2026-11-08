import { describe, expect, it } from 'vitest';
import { clampCenter, fitRect, panBy, toggleAt, zoomedRect } from './secondViewZoom';

const area: [number, number] = [3840, 2160];
const shown: [number, number] = [6000, 4000];

describe('second window zoom', () => {
  it('fits the photograph contained and centred', () => {
    expect(fitRect(area, shown)).toEqual([300, 0, 3240, 2160]);
  });

  it('zooms to 100 % keeping the clicked point under the pointer', () => {
    // The photograph's centre clicked: 100 % centred there.
    expect(toggleAt(area, shown, null, [1920, 1080])).toEqual({ scale: 1, center: [0.5, 0.5] });
    // A point at three quarters of the width stays under the pointer.
    const point: [number, number] = [300 + 3240 * 0.75, 1080];
    const zoom = toggleAt(area, shown, null, point)!;
    const screenX = area[0] / 2 + (0.75 - zoom.center[0]) * shown[0] * zoom.scale;
    expect(screenX).toBeCloseTo(point[0], 3);
    // A second click fits again.
    expect(toggleAt(area, shown, zoom, point)).toBeNull();
  });

  it('never pans past the photograph edge', () => {
    const corner = toggleAt(area, shown, null, [300, 0])!;
    expect(corner.center[0]).toBeCloseTo(1920 / 6000);
    expect(corner.center[1]).toBeCloseTo(1080 / 4000);
    const panned = panBy(area, shown, { scale: 1, center: [0.5, 0.5] }, [100000, 0]);
    expect(panned.center[0]).toBeCloseTo(1920 / 6000);
    // Smaller than the window along an axis: centred there.
    expect(clampCenter([8000, 2160], shown, { scale: 1, center: [0.1, 0.5] }).center[0]).toBe(0.5);
  });

  it('pans by the pointer movement at the magnification', () => {
    const panned = panBy(area, shown, { scale: 1, center: [0.5, 0.5] }, [600, -400]);
    expect(panned.center[0]).toBeCloseTo(0.5 - 600 / 6000);
    expect(panned.center[1]).toBeCloseTo(0.5 + 400 / 4000);
  });

  it('places the whole photograph and a region at the zoom as the native display does', () => {
    const zoom = { scale: 1, center: [0.5, 0.5] as [number, number] };
    // 100 % centred: the photograph's centre at the window's centre.
    expect(zoomedRect(area, shown, null, zoom)).toEqual([1920 - 3000, 1080 - 2000, 6000, 4000]);
    // A region of it sits at its own place in the photograph.
    expect(zoomedRect(area, shown, [2000, 1500, 3840, 2160], zoom)).toEqual([1920 - 1000, 1080 - 500, 3840, 2160]);
    // At half scale both halve around the window's centre.
    const half = { scale: 0.5, center: [0.25, 0.5] as [number, number] };
    expect(zoomedRect(area, shown, null, half)).toEqual([1920 - 750, 1080 - 1000, 3000, 2000]);
  });
});
