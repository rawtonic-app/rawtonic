import '../i18n';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { DEFAULT_RECIPE_DRAFT, exportSettingsFromDraft } from './exportRecipes';
import { exportJobLabel, notifyExportOutputs, outputFolder } from './exportFeedback';

describe('export queue labels', () => {
  it('distinguishes recipes, size, format and destination', () => {
    const label = exportJobLabel('Fast (Web)', { ...DEFAULT_RECIPE_DRAFT, resize: true, resizeMode: 'longEdge', resizeValue: 1600 }, 'jpg', '/delivery/web');
    expect(label).toContain('Fast (Web)');
    expect(label).toContain('1600');
    expect(label).toContain('JPG');
    expect(label).toContain('/delivery/web');
    const withSource = exportJobLabel('Web', DEFAULT_RECIPE_DRAFT, 'jpg', '/out', ['C:\\photos\\a.CR3?vc=1']);
    expect(withSource).toBe('a.CR3 · Web · JPG · Full resolution · /out');
  });
  it('uses the size actually sent to the export engine', () => {
    const draft = { ...DEFAULT_RECIPE_DRAFT, resize: true, resizeMode: 'longEdge' as const, resizeValue: 9999999 };
    const actual = exportSettingsFromDraft(draft).settings.resize!.value;
    expect(exportJobLabel('Web', draft, 'jpg', '/out')).toContain(String(actual));
  });
  it('names a subfolder only when one is given, never a trailing slash', () => {
    const named = exportJobLabel('Web', { ...DEFAULT_RECIPE_DRAFT, subfolder: ' Web/small ' }, 'jpg', '/out');
    expect(named).toBe('Web · JPG · Full resolution · /out / Web/small');
    for (const subfolder of [' ', '   ', '\t\n']) {
      const label = exportJobLabel('Web', { ...DEFAULT_RECIPE_DRAFT, subfolder }, 'jpg', '/out');
      expect(label).toBe('Web · JPG · Full resolution · /out');
      expect(label).not.toContain('/out /');
    }
  });
});


vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(undefined) }));
vi.mock('react-toastify', () => ({ toast: { success: vi.fn(), error: vi.fn() } }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

it('shows one notification for a run with a reveal action for each output folder', () => {
  notifyExportOutputs(['/delivery/web/a.jpg', '/delivery/web/b.jpg', '/delivery/print/a.tif']);
  expect(toast.success).toHaveBeenCalledTimes(1);
  const content = vi.mocked(toast.success).mock.calls[0][0];
  render(content as React.ReactElement);
  expect(screen.getByText('/delivery/web')).toBeTruthy();
  expect(screen.getByText('/delivery/print')).toBeTruthy();
  const buttons = screen.getAllByRole('button');
  expect(buttons).toHaveLength(2);
  fireEvent.click(buttons[0]);
  expect(invoke).toHaveBeenCalledWith('show_in_finder', { path: '/delivery/web/a.jpg' });
});

it('handles native output folder roots', () => {
  expect(outputFolder('/a.jpg')).toBe('/');
  expect(outputFolder('C:\\a.jpg')).toBe('C:\\');
  expect(outputFolder('C:\\out\\a.jpg')).toBe('C:\\out');
});
