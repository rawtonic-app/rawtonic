import '../../i18n';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { ZoomSelect } from './ViewerControls';
import { useSettingsStore } from '../../store/useSettingsStore';
import type { AppSettings } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => {}) }));

beforeEach(() => useSettingsStore.setState({ appSettings: {} as AppSettings }));
afterEach(cleanup);

it('lists Fit and every level, remembers the level chosen and gives the keys back to the viewer (ZOOM-Z)', () => {
  const onZoom = vi.fn();
  render(<ZoomSelect onZoom={onZoom} />);
  const select = screen.getByRole('combobox', { name: 'Zoom' }) as HTMLSelectElement;
  expect([...select.options].map((o) => o.textContent)).toEqual([
    'Zoom',
    'Fit',
    '25%',
    '33%',
    '50%',
    '66%',
    '100%',
    '200%',
    '400%',
    '800%',
    '1600%'
  ]);
  select.focus();
  fireEvent.change(select, { target: { value: '0.25' } });
  expect(onZoom).toHaveBeenLastCalledWith(0.25, false, true);
  expect(useSettingsStore.getState().appSettings?.viewerZoomLevel).toBe(0.25);
  // Z would otherwise go to the focused select (its type-ahead).
  expect(document.activeElement).not.toBe(select);
  fireEvent.change(select, { target: { value: 'fit' } });
  expect(onZoom).toHaveBeenLastCalledWith(1, true);
  expect(useSettingsStore.getState().appSettings?.viewerZoomLevel).toBe(0.25);
});
