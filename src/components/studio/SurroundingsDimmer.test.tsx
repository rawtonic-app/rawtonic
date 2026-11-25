import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useEditorStore } from '../../store/useEditorStore';
import { useUIStore } from '../../store/useUIStore';
import { SurroundingsDimmer } from './SurroundingsDimmer';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  useUIStore.setState({ activeView: 'library', surroundings: 'normal' });
  useEditorStore.setState({ selectedImage: null });
});

it('cuts a transparent hole clipped to the viewer and follows changes in photo placement', () => {
  let frame: FrameRequestCallback;
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    frame = callback;
    return 1;
  });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
  const click = vi.fn();
  useUIStore.setState({ activeView: 'editor', surroundings: 'dim' });
  useEditorStore.setState({ selectedImage: { path: '/photo.raw' } as never });
  const { getByTestId, getByRole } = render(
    <>
      <div data-surroundings-clip="" data-testid="viewer">
        <div data-surroundings-photo="" data-testid="photo" />
      </div>
      <button onClick={click}>Underlying control</button>
      <SurroundingsDimmer />
    </>
  );
  getByTestId('viewer').getBoundingClientRect = () => new DOMRect(10, 50, 300, 200);
  const photo = getByTestId('photo');
  photo.getBoundingClientRect = () => new DOMRect(60, 80, 200, 140);
  act(() => frame(0));
  const dimmer = getByTestId('surroundings-dimmer');
  const hole = () => dimmer.querySelector('mask rect[fill="black"]')!;
  expect(hole().getAttribute('x')).toBe('60');
  expect(hole().getAttribute('width')).toBe('200');
  expect(dimmer.querySelector('rect[mask]')?.getAttribute('opacity')).toBe('0.8');
  expect(dimmer.style.pointerEvents).toBe('none');
  fireEvent.click(getByRole('button'));
  expect(click).toHaveBeenCalledOnce();

  photo.getBoundingClientRect = () => new DOMRect(-100, 65, 350, 300);
  act(() => frame(16));
  expect(hole().getAttribute('x')).toBe('10');
  expect(hole().getAttribute('width')).toBe('240');
  expect(hole().getAttribute('height')).toBe('185');

  act(() => useUIStore.setState({ surroundings: 'black' }));
  expect(dimmer.querySelector('rect[mask]')?.getAttribute('opacity')).toBe('1');
});
