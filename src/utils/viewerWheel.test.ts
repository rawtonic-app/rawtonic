import { afterEach, describe, expect, it, vi } from 'vitest';
import { listenForPinch, wheelAction } from './viewerWheel';

const wheel = (init: Partial<WheelEvent>) => ({
  deltaX: 0,
  deltaY: 0,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  ...init
});

afterEach(() => {
  delete window.__rawtonicScrollPrecise;
});

describe('wheelAction', () => {
  it('pans for a scroll the platform reports as precise, whatever the preference (UXR4M-01)', () => {
    window.__rawtonicScrollPrecise = true;
    expect(wheelAction(wheel({ deltaY: 12 }), 'mouse')).toEqual({ kind: 'pan', dx: 0, dy: 12 });
    // A sideways swipe pans sideways, never zooms.
    expect(wheelAction(wheel({ deltaX: -9 }), 'mouse')).toEqual({ kind: 'pan', dx: -9, dy: 0 });
    expect(wheelAction(wheel({ deltaX: 3, deltaY: 4 }), undefined)).toEqual({ kind: 'pan', dx: 3, dy: 4 });
  });

  it('zooms for a mouse wheel the platform reports, even with the touchpad preference', () => {
    window.__rawtonicScrollPrecise = false;
    const action = wheelAction(wheel({ deltaY: 40 }), 'trackpad');
    expect(action.kind).toBe('zoom');
    expect(action.kind === 'zoom' && action.factor).toBeLessThan(1);
    expect(wheelAction(wheel({ deltaY: 40, shiftKey: true }), 'trackpad')).toEqual({ kind: 'pan', dx: 40, dy: 0 });
  });

  it('never zooms from deltaX', () => {
    expect(wheelAction(wheel({ deltaX: 40 }), 'mouse')).toEqual({ kind: 'pan', dx: 40, dy: 0 });
    const pinch = wheelAction(wheel({ deltaX: 40, ctrlKey: true }), 'mouse');
    expect(pinch).toEqual({ kind: 'zoom', factor: 1 });
  });

  it('without a platform report, follows the preference; ctrlKey (a pinch) always zooms', () => {
    expect(wheelAction(wheel({ deltaY: 10 }), 'trackpad')).toEqual({ kind: 'pan', dx: 0, dy: 10 });
    expect(wheelAction(wheel({ deltaY: 10 }), 'mouse').kind).toBe('zoom');
    const pinch = wheelAction(wheel({ deltaY: -2, ctrlKey: true }), 'trackpad');
    expect(pinch.kind === 'zoom' && pinch.factor).toBeGreaterThan(1);
  });
});

describe('listenForPinch', () => {
  const gesture = (type: string, scale: number, clientX = 10, clientY = 20) => {
    const event = new Event(type, { cancelable: true }) as Event & { scale: number; clientX: number; clientY: number };
    Object.assign(event, { scale, clientX, clientY });
    return event;
  };

  it('zooms by each change of a WebKit pinch about its point', () => {
    const element = document.createElement('div');
    const onZoom = vi.fn();
    const stop = listenForPinch(
      element,
      onZoom,
      () => null,
      () => 1
    );
    element.dispatchEvent(gesture('gesturestart', 1));
    const change = gesture('gesturechange', 1.25);
    element.dispatchEvent(change);
    element.dispatchEvent(gesture('gesturechange', 1.5));
    expect(change.defaultPrevented).toBe(true);
    expect(onZoom).toHaveBeenNthCalledWith(1, 1.25, 10, 20);
    expect(onZoom.mock.calls[1][0]).toBeCloseTo(1.2);
    // A new gesture starts from 1 again.
    element.dispatchEvent(gesture('gesturestart', 1));
    element.dispatchEvent(gesture('gesturechange', 0.5));
    expect(onZoom).toHaveBeenLastCalledWith(0.5, 10, 20);
    stop();
    element.dispatchEvent(gesture('gesturechange', 2));
    expect(onZoom).toHaveBeenCalledTimes(3);
  });

  it('scales a pinch by the zoom speed preference, as the wheel', () => {
    const element = document.createElement('div');
    const factors: number[] = [];
    let speed = 0.1;
    const stop = listenForPinch(
      element,
      (factor) => factors.push(factor),
      () => null,
      () => speed
    );
    element.dispatchEvent(gesture('gesturestart', 1));
    element.dispatchEvent(gesture('gesturechange', 2));
    speed = 3;
    element.dispatchEvent(gesture('gesturestart', 1));
    element.dispatchEvent(gesture('gesturechange', 2));
    expect(factors[0]).toBeCloseTo(2 ** 0.1);
    expect(factors[1]).toBeCloseTo(8);
    stop();
  });
});
