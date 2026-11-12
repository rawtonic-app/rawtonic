// SPDX-License-Identifier: AGPL-3.0-or-later
// WPFC: a 282 s compile after Auto was shown only in the top bar's activity
// slot, which was off screen; the viewer says it too.
import '../../i18n';
import { act, render, screen } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';
import { useActivityStore } from '../../store/useActivityStore';
import GraphicsPreparingNotice from './GraphicsPreparingNotice';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn() }));

beforeEach(() => useActivityStore.setState({ entries: [] }));

it('shows while a compile runs and goes when it ends', () => {
  render(<GraphicsPreparingNotice />);
  expect(screen.queryByRole('status')).toBeNull();
  act(() => {
    useActivityStore.getState().start({ id: 'gpu-compiling', source: 'app', title: 'Preparing graphics' });
  });
  expect(screen.getByRole('status').textContent).toContain('Preparing graphics for this computer');
  act(() => useActivityStore.getState().resolve('gpu-compiling'));
  expect(screen.queryByRole('status')).toBeNull();
});
