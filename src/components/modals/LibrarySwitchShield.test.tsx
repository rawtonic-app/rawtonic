import '../../i18n';
import { afterEach, expect, it } from 'vitest';
import { act, cleanup, render } from '@testing-library/react';
import LibrarySwitchShield from './LibrarySwitchShield';
import { useUIStore } from '../../store/useUIStore';

afterEach(() => {
  cleanup();
  useUIStore.setState({ switchingLibrary: false });
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
