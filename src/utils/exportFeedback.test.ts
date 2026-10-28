import { describe, expect, it } from 'vitest';
import { DEFAULT_RECIPE_DRAFT } from './exportRecipes';
import { exportJobLabel } from './exportFeedback';

describe('export queue labels', () => {
  it('distinguishes recipes, size, format and destination', () => {
    const label = exportJobLabel('Fast (Web)', { ...DEFAULT_RECIPE_DRAFT, resize: true, resizeMode: 'longEdge', resizeValue: 1600 }, 'jpg', '/delivery/web');
    expect(label).toContain('Fast (Web)');
    expect(label).toContain('1600');
    expect(label).toContain('JPG');
    expect(label).toContain('/delivery/web');
  });
});

import '../i18n';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { afterEach, vi } from 'vitest';
import { notifyExportOutputs } from './exportFeedback';

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
