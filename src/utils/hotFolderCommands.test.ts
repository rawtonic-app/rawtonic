import { expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

import { hotFolderCommand } from './hotFolderCommands';

it('sends each hot folder command only after the one before finished, also a failed one', async () => {
  const answers: Array<{ command: string; resolve: (v: unknown) => void; reject: (e: unknown) => void }> = [];
  invoke.mockImplementation(
    (command: string) => new Promise((resolve, reject) => answers.push({ command, resolve, reject })),
  );
  // Leaving A, opening B, back to A.
  const sent = [
    hotFolderCommand('hot_folder_suspend', { directory: '/A' }),
    hotFolderCommand('hot_folder_resume', { directory: '/B' }),
    hotFolderCommand('hot_folder_suspend', { directory: '/B' }),
    hotFolderCommand('hot_folder_resume', { directory: '/A' }),
  ];
  const settle = () => new Promise((r) => setTimeout(r, 0));
  await settle();
  expect(answers.map((a) => a.command)).toEqual(['hot_folder_suspend']);
  answers[0].resolve(null);
  await settle();
  expect(answers).toHaveLength(2);
  answers[1].reject('library gone');
  await settle();
  expect(answers).toHaveLength(3);
  answers[2].resolve(null);
  await settle();
  expect(answers.map((a) => a.command)).toEqual([
    'hot_folder_suspend',
    'hot_folder_resume',
    'hot_folder_suspend',
    'hot_folder_resume',
  ]);
  expect(invoke.mock.calls[3][1]).toEqual({ directory: '/A' });
  answers[3].resolve({ folder: '/hot' });
  await expect(sent[1]).rejects.toBe('library gone');
  await expect(sent[3]).resolves.toEqual({ folder: '/hot' });
});
