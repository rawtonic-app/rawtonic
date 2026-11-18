import '../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import SessionWorkspace, { type SessionSummary } from '../components/session/SessionWorkspace';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useUIStore } from '../store/useUIStore';
import { ContextMenuProvider } from '../context/ContextMenuContext';
import type { AppSettings } from '../components/ui/AppProperties';
import { expectNoNewViolations, type Allowed } from '../utils/axeAudit.testSupport';

type DragDropPayload = { type: 'drop'; paths: string[]; position: { x: number; y: number } };
const drag = vi.hoisted(() => ({ handler: null as null | ((event: { payload: DragDropPayload }) => void) }));

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true, invoke: vi.fn() }));
vi.mock('@tauri-apps/api/webview', () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async (handler: (event: { payload: DragDropPayload }) => void) => {
      drag.handler = handler;
      return () => {};
    }
  })
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => null) }));
vi.mock('@tauri-apps/api/path', () => ({ join: async (...parts: string[]) => parts.join('/') }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

const summary: SessionSummary = {
  directory: '/jobs/Acme',
  manifest: { name: 'Acme', mode: 'session', capture: 'Capture', output: 'Output' },
  assets: 0,
  picked: 0,
  rated: 0,
  xmp_pending: 0
};
const scanned = [
  { path: '/Volumes/CARD/DCIM/A.ARW', size: 20_000_000, duplicate: false },
  { path: '/Volumes/CARD/DCIM/B.ARW', size: 21_000_000, duplicate: true },
  { path: '/Volumes/CARD/DCIM/C.ARW', size: 22_000_000, duplicate: false }
];

beforeEach(() => {
  drag.handler = null;
  document.body.innerHTML = '';
  useUIStore.setState({ activeView: 'library' });
  useLibraryStore.setState({
    rootPaths: [],
    imageList: [],
    multiSelectedPaths: [],
    imageFlags: {},
    sessionDirectory: null
  });
  useSettingsStore.setState({
    appSettings: {} as AppSettings,
    handleSettingsChange: async () => {},
    supportedTypes: { raw: ['arw'], nonRaw: ['jpg'] }
  });
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute('open', '');
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute('open');
  };
  vi.mocked(invoke).mockImplementation(async (command) => {
    switch (command) {
      case 'session_last':
      case 'session_summary':
        return summary;
      case 'session_flags':
        return {};
      case 'session_ingest_scan':
        return { files: scanned, truncated: false };
      case 'session_ingest_thumbnail':
      case 'session_ingest_preview':
        return new ArrayBuffer(0);
      case 'session_ingest_capture_info':
        return { iso: 'ISO 200', exposure: '1/500 s', aperture: 'f/4', focalLength: '85 mm' };
      default:
        return { path: '/jobs/Acme', children: [] };
    }
  });
});
afterEach(cleanup);

const ALLOWED: Allowed[] = [];

it('Import dialog (card dropped on the window) has no new accessibility violations', async () => {
  render(
    <ContextMenuProvider>
      <SessionWorkspace onNavigate={vi.fn(async () => {})} />
    </ContextMenuProvider>
  );
  await waitFor(() => expect(drag.handler).not.toBeNull());
  document.elementFromPoint = (() => null) as unknown as typeof document.elementFromPoint;
  act(() => drag.handler!({ payload: { type: 'drop', paths: ['/Volumes/CARD/DCIM'], position: { x: 10, y: 10 } } }));
  await screen.findByRole('button', { name: 'Copy 2 photos' });
  await act(async () => {});
  await expectNoNewViolations('import', ALLOWED);
});
