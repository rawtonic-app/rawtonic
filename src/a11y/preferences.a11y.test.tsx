import '../i18n';
import { afterEach, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import SettingsPanel from '../components/panel/SettingsPanel';
import PreferencesDialog from '../components/studio/PreferencesDialog';
import { useSettingsStore } from '../store/useSettingsStore';
import type { AppSettings } from '../components/ui/AppProperties';
import { expectNoNewViolations, type Allowed } from '../utils/axeAudit.testSupport';

vi.mock('@tauri-apps/api/core', () => ({
  isTauri: () => false,
  invoke: vi.fn(() => Promise.resolve([])),
  convertFileSrc: (p: string) => p
}));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0') }));

Object.defineProperty(window, 'matchMedia', {
  configurable: true,
  value: () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })
});

afterEach(cleanup);

const ALLOWED: Allowed[] = [];

it('Preferences: every category has no new accessibility violations', async () => {
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: 'macos' });
  render(
    <PreferencesDialog>
      <SettingsPanel
        appSettings={{ keybinds: {} } as unknown as AppSettings}
        onBack={vi.fn()}
        onLibraryRefresh={vi.fn()}
        onSettingsChange={vi.fn()}
        rootPaths={[]}
      />
    </PreferencesDialog>
  );
  await act(async () => {});
  const categories = screen
    .getAllByRole('button')
    .filter((b) => b.hasAttribute('aria-pressed') && b.closest('nav, [role="tablist"], aside'));
  for (const category of categories) {
    const name = category.textContent ?? '';
    await act(async () => fireEvent.click(category));
    await expectNoNewViolations(`preferences ${name}`, ALLOWED);
  }
});

it('names each shortcut button by its action and key, and says when an action has no key', async () => {
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: 'macos' });
  render(
    <SettingsPanel
      appSettings={{ keybinds: {} } as unknown as AppSettings}
      onBack={vi.fn()}
      onLibraryRefresh={vi.fn()}
      onSettingsChange={vi.fn()}
      rootPaths={[]}
    />
  );
  await act(async () => fireEvent.click(screen.getByRole('button', { name: 'Controls' })));
  expect(screen.getByRole('button', { name: 'Undo adjustment ⌘ + Z' })).toBeTruthy();
  // No default key: the key cap reads "Not assigned" instead of staying empty.
  expect(screen.getByRole('button', { name: 'Copy selected file(s) Not assigned' })).toBeTruthy();
});
