import '../i18n';
import { afterEach, beforeEach, it, vi } from 'vitest';
import { act, cleanup, render } from '@testing-library/react';
import StudioExport from '../components/studio/StudioExport';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Status } from '../components/ui/ExportImportProperties';
import { expectNoNewViolations, type Allowed } from '../utils/axeAudit.testSupport';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn((command: string, args?: { directory?: string }) =>
    Promise.resolve(command === 'session_usable_folder' ? `${args?.directory}/Output` : undefined)
  )
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

beforeEach(() => {
  localStorage.clear();
  useSettingsStore.setState({ appSettings: null });
  useLibraryStore.setState({ multiSelectedPaths: ['/p/a.RAF', '/p/b.RAF'], sessionDirectory: null });
  useProcessStore.setState({
    exportQueue: [],
    exportQueueHeld: false,
    exportItems: {},
    exportPausing: [],
    exportState: { status: Status.Idle, errorMessage: '', progress: { current: 0, total: 0 } }
  });
});
afterEach(cleanup);

const ALLOWED: Allowed[] = [];

it('Export dialog has no new accessibility violations', async () => {
  render(<StudioExport />);
  await act(async () => {});
  document.querySelectorAll('details').forEach((group) => {
    group.open = true;
  });
  await expectNoNewViolations('export', ALLOWED);
});
