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

/** Settings that bring out the lists and chips: AI tagging on, a saved lens, tags and shortcuts. */
const SEEDED = {
  keybinds: {},
  enableAiTagging: true,
  myLenses: [{ maker: 'Nikon', model: 'AF-S 24-70mm f/2.8G' }],
  customAiTags: ['harbour', 'portrait'],
  taggingShortcuts: ['client', 'select', 'web']
} as unknown as AppSettings;

/** Preferences as the app wires it: settings from the store, saved through it. */
function Wired() {
  const appSettings = useSettingsStore((s) => s.appSettings);
  return (
    <SettingsPanel
      appSettings={appSettings as AppSettings}
      onBack={vi.fn()}
      onLibraryRefresh={vi.fn()}
      onSettingsChange={async (next) => useSettingsStore.setState({ appSettings: next })}
      rootPaths={[]}
    />
  );
}

it('Preferences: every category has no new accessibility violations', async () => {
  useSettingsStore.setState({ appSettings: SEEDED, osPlatform: 'macos' });
  render(
    <PreferencesDialog>
      <Wired />
    </PreferencesDialog>
  );
  await act(async () => {});
  // The seeded chips and the saved lens are on the page being audited.
  await act(async () => fireEvent.click(screen.getByRole('button', { name: 'General' })));
  expect(screen.getByRole('button', { name: 'Remove shortcut "web"' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Remove tag "harbour"' })).toBeTruthy();
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

it('keeps the keyboard in the list when a tagging shortcut or tag chip is removed', async () => {
  useSettingsStore.setState({ appSettings: SEEDED, osPlatform: 'macos' });
  render(
    <PreferencesDialog>
      <Wired />
    </PreferencesDialog>
  );
  await act(async () => fireEvent.click(screen.getByRole('button', { name: 'General' })));
  const chip = (name: string) => screen.getByRole('button', { name: `Remove shortcut "${name}"` });
  // Middle chip: the next one takes the focus.
  chip('select').focus();
  await act(async () => fireEvent.click(chip('select')));
  expect(document.activeElement).toBe(chip('web'));
  // Last chip: the one before it.
  await act(async () => fireEvent.click(chip('web')));
  expect(document.activeElement).toBe(chip('client'));
  // Only chip: the field that adds shortcuts.
  await act(async () => fireEvent.click(chip('client')));
  expect(document.activeElement).toBe(screen.getByPlaceholderText('Add shortcuts (comma separated)...'));
});
