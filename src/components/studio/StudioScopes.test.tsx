import '../../i18n';
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import StudioScopes, { scopeImage, scopeMode, waveformY } from './StudioScopes';
import { useEditorStore } from '../../store/useEditorStore';
import type { WaveformData } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (id: string, scheme: string) => `${scheme}://localhost/${id}`
}));

afterEach(() => {
  cleanup();
  useEditorStore.setState({ waveform: null, activeWaveformChannel: 'luma' });
});

const data: WaveformData = {
  width: 256,
  height: 256,
  luma: '0b5f2c1e-8d3a-4f6b-9c2d-7e1a3b5c9d0f',
  rgb: '',
  parade: '',
  vectorscope: '',
  vectorscopeTargets: [
    { name: 'R', percent: 100, x: 104.9, y: 28 },
    { name: 'R', percent: 75, x: 110.8, y: 53 }
  ],
  skinLineDegrees: 125.5
};

it('graticule lines sit on the codes they name: 100 % on code 255, 0 % on code 0', () => {
  // Row r holds code 255 - r; its centre is r + 0.5.
  expect(waveformY(100)).toBe(0.5);
  expect(waveformY(0)).toBe(255.5);
  expect(waveformY(50)).toBe(128);
});

it('shows a computed scope by its URL and nothing for a mode that was not computed', () => {
  // The image itself never comes through the event: only its id (scope_images.rs).
  expect(scopeImage(data, 'luma')).toBe('rawtonic-scope://localhost/0b5f2c1e-8d3a-4f6b-9c2d-7e1a3b5c9d0f');
  // Not an id (e.g. an image inline as before): never put into a URL.
  expect(scopeImage({ ...data, luma: 'iVBORw0KGgo=' }, 'luma')).toBeNull();
  expect(scopeImage({ ...data, luma: '../17' }, 'luma')).toBeNull();
  expect(scopeImage(data, 'rgb')).toBeNull();
  expect(scopeImage(null, 'luma')).toBeNull();
  // An image of another size is never drawn.
  expect(scopeImage({ ...data, width: 128 }, 'luma')).toBeNull();
  expect(scopeMode('parade')).toBe('parade');
  expect(scopeMode('bogus')).toBe('luma');
});

it("switches modes and draws the backend's vectorscope targets and skin line", () => {
  useEditorStore.setState({ waveform: data });
  const { container } = render(<StudioScopes />);
  expect(screen.getByRole('img', { name: 'Luma waveform of the shown photo' })).toBeTruthy();
  expect(container.querySelector('.studio-scope-plot > img')?.getAttribute('src')).toBe(
    'rawtonic-scope://localhost/0b5f2c1e-8d3a-4f6b-9c2d-7e1a3b5c9d0f'
  );
  expect(container.querySelectorAll('[data-percent]').length).toBe(5);
  fireEvent.click(screen.getByRole('button', { name: 'Vector' }));
  expect(useEditorStore.getState().activeWaveformChannel).toBe('vectorscope');
  // Not computed yet: the plot says so instead of showing a stale mode.
  expect(screen.getByRole('img', { name: 'Scopes available after opening a photo' })).toBeTruthy();
  expect(container.querySelector('.studio-scope-plot > img')).toBeNull();
  const red = container.querySelector('[data-target="R100"]')!;
  expect(Number(red.getAttribute('x')) + Number(red.getAttribute('width')) / 2).toBeCloseTo(104.9);
  expect(container.querySelector('[data-target="R75"]')).not.toBeNull();
  const skin = container.querySelector('.studio-scope-skin')!;
  const angle = Math.atan2(128 - Number(skin.getAttribute('y2')), Number(skin.getAttribute('x2')) - 128);
  expect((angle * 180) / Math.PI).toBeCloseTo(125.5);
});

it('keeps Space and Enter on a mode button away from the global shortcuts', () => {
  // useKeyboardShortcuts listens on window: Space there zooms the photo and
  // prevents the button's own activation.
  const reached: string[] = [];
  const shortcuts = (event: KeyboardEvent) => reached.push(event.key);
  window.addEventListener('keydown', shortcuts);
  try {
    render(<StudioScopes />);
    const vector = screen.getByRole('button', { name: 'Vector' });
    for (const key of [' ', 'Enter', 'j']) {
      expect(fireEvent.keyDown(vector, { key })).toBe(true);
    }
    expect(reached).toEqual(['j']);
  } finally {
    window.removeEventListener('keydown', shortcuts);
  }
});
