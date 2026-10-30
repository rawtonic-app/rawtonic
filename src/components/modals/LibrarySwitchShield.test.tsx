import '../../i18n';
import { afterEach, expect, it } from 'vitest';
import { act, cleanup, render } from '@testing-library/react';
import LibrarySwitchShield from './LibrarySwitchShield';
import { useUIStore } from '../../store/useUIStore';

afterEach(() => {
  cleanup();
  useUIStore.setState({ switchingLibrary: false, quitSaving: null });
});

it('holds the app in a modal dialog while the next library opens', () => {
  render(<LibrarySwitchShield />);
  // Shortcuts and menu commands are held while a dialog is open.
  expect(document.querySelector('dialog[open]')).toBeNull();
  act(() => useUIStore.setState({ switchingLibrary: true }));
  expect(document.querySelector('dialog[open]')?.textContent).toBe('Opening library…');
  act(() => useUIStore.setState({ switchingLibrary: false }));
  expect(document.querySelector('dialog[open]')).toBeNull();
});

it('holds the app the same way while a quit saves, with its progress', () => {
  render(<LibrarySwitchShield />);
  act(() => useUIStore.setState({ quitSaving: 'Saving changes before Rawtonic quits… (3/7)' }));
  expect(document.querySelector('dialog[open]')?.textContent).toBe('Saving changes before Rawtonic quits… (3/7)');
  act(() => useUIStore.setState({ quitSaving: null }));
  expect(document.querySelector('dialog[open]')).toBeNull();
});
