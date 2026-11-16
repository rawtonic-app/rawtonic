import { expect, it, vi } from 'vitest';
import { openPreviousImport } from './folderSync';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async () => ({ batch: null, paths: ['/job/a.CR3', '/job/a.CR3?vc=abc123'] }))
}));

// UXR4M-19: a refresh of Previous Import (after a new virtual copy) ends only
// once the view shows the new list, so what follows (selecting the copy)
// reads that list and not the old one.
it('resolves only after the view has shown its photos', async () => {
  const order: string[] = [];
  const select = vi.fn(async () => {
    await new Promise((resolve) => setTimeout(resolve, 5));
    order.push('shown');
  });
  const count = await openPreviousImport('/job', 'Previous Import', select, true);
  order.push('returned');
  expect(select).toHaveBeenCalledWith(
    'session-previous-import',
    'Previous Import',
    ['/job/a.CR3', '/job/a.CR3?vc=abc123'],
    true
  );
  expect(order).toEqual(['shown', 'returned']);
  expect(count).toBe(2);
});
