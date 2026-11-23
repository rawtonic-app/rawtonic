import '../i18n';
import { afterEach, beforeEach, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import StudioDevelop from '../components/studio/StudioDevelop';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { INITIAL_ADJUSTMENTS } from '../utils/adjustments';
import { Panel, type SelectedImage } from '../components/ui/AppProperties';
import { ContextMenuProvider } from '../context/ContextMenuContext';
import { expectNoNewViolations, KNOWN, type Allowed } from '../utils/axeAudit.testSupport';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn((name: string) => Promise.resolve(name === 'load_metadata' ? {} : undefined))
}));

beforeEach(() => {
  vi.stubGlobal('PointerEvent', MouseEvent);
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    }
  );
  useEditorStore.setState({
    ...useEditorStore.getInitialState(),
    adjustments: structuredClone(INITIAL_ADJUSTMENTS),
    selectedImage: { path: '/qa/photo.dng', width: 6000, height: 4000, isRaw: true, isReady: true } as SelectedImage
  });
  useUIStore.setState({ activePanel: Panel.Adjustments });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const ALLOWED: Allowed[] = [KNOWN.toolHeader];

// Each tab runs a full axe scan of the expanded Develop DOM, which can exceed 5 s in total on loaded hosts.
it(
  'Develop: every tool tab, with its groups open, has no new accessibility violations',
  { timeout: 15_000 },
  async () => {
    render(<StudioDevelop />, { wrapper: ContextMenuProvider });
    const tabs = within(screen.getByRole('navigation')).getAllByRole('button');
    for (const tab of tabs) {
      const name = tab.getAttribute('aria-label') ?? tab.textContent ?? '';
      act(() => fireEvent.click(tab));
      document.querySelectorAll('details').forEach((group) => {
        group.open = true;
      });
      await expectNoNewViolations(`develop ${name}`, ALLOWED);
    }
  }
);
