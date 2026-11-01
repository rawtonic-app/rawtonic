import '../../i18n';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import CompareButton from './CompareButton';
import { useEditorStore } from '../../store/useEditorStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import type { Option } from '../ui/AppProperties';
const { showContextMenu } = vi.hoisted(() => ({ showContextMenu: vi.fn() }));
vi.mock('../../context/ContextMenuContext', () => ({ useContextMenu: () => ({ showContextMenu }) }));
beforeEach(() => {
  showContextMenu.mockClear();
  useSettingsStore.setState({ appSettings: { keybinds: {} } as never, osPlatform: 'macos' });
  useEditorStore.setState({ selectedImage: { path: '/a.dng', isReady: true } as never, splitView: true, splitOrientation: 'left-right', splitPosition: 0.3, zoom: 3 });
});
afterEach(cleanup);
it('opens the same checked choices by chevron and right-click, selects without changing zoom', () => {
  render(<CompareButton disabled={false} title="Before/After" />);
  fireEvent.click(screen.getByRole('button', { name: 'Compare mode' }));
  const choices = showContextMenu.mock.calls[0][2] as Option[];
  expect(choices.map((o) => [o.label, o.checked])).toEqual([
    ['Off', false], ['Split left/right', true], ['Split top/bottom', false], ['Side by side', false],
  ]);
  act(() => choices[2].onClick!());
  expect(useEditorStore.getState()).toMatchObject({ splitView: true, splitOrientation: 'top-bottom', splitPosition: 0.3, zoom: 3 });
  fireEvent.contextMenu(screen.getByRole('button', { name: 'Before/After' }));
  const next = showContextMenu.mock.calls[1][2] as Option[];
  expect(next[2].checked).toBe(true);
  act(() => next[0].onClick!());
  expect(useEditorStore.getState().splitView).toBe(false);
  fireEvent.click(screen.getByRole('button', { name: 'Before/After' }));
  expect(useEditorStore.getState()).toMatchObject({ splitView: true, splitOrientation: 'top-bottom' });
});
it('disables both controls and guards a menu left open while the photo unloads', () => {
  const { rerender } = render(<CompareButton disabled={false} title="Before/After" />);
  fireEvent.click(screen.getByRole('button', { name: 'Compare mode' }));
  const choices = showContextMenu.mock.calls[0][2] as Option[];
  useEditorStore.setState({ selectedImage: null });
  act(() => choices[2].onClick!());
  expect(useEditorStore.getState().splitOrientation).toBe('left-right');
  rerender(<CompareButton disabled title="Before/After" />);
  for (const button of screen.getAllByRole('button')) expect(button.hasAttribute('disabled')).toBe(true);
});

it('shows a reassigned cycle key in the menu button tooltip', () => {
  useSettingsStore.setState({ appSettings: { keybinds: { cycle_compare_mode: ['alt', 'KeyY'] } } as never });
  render(<CompareButton disabled={false} title="Before/After" />);
  expect(screen.getByRole('button', { name: 'Compare mode' }).title).toBe('Compare mode (⌥ + Y)');
});
