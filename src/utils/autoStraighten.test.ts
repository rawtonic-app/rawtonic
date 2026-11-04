import '../i18n';
import { beforeEach, expect, it, vi } from 'vitest';

const { toast } = vi.hoisted(() => ({ toast: { info: vi.fn(), error: vi.fn() } }));
vi.mock('../activity/notify', () => ({ notify: toast }));

import { runAutoStraighten } from './autoStraighten';

beforeEach(() => {
  toast.info.mockReset();
  toast.error.mockReset();
});

it("shows a failed Auto straighten's message, from a command error or a plain string", async () => {
  await runAutoStraighten(() => Promise.reject({ code: 'internal', message: 'Auto straighten failed: panic' }));
  expect(toast.error).toHaveBeenCalledTimes(1);
  expect(toast.error.mock.calls[0][0]).toContain('Auto straighten failed: panic');
  expect(toast.error.mock.calls[0][0]).not.toContain('[object Object]');
  await runAutoStraighten(() => Promise.reject('No image loaded'));
  expect(toast.error.mock.calls[1][0]).toContain('No image loaded');
});
