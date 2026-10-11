// @vitest-environment node
import { afterEach, expect, it, vi } from 'vitest';
import { open } from '@tauri-apps/plugin-dialog';
import { build } from 'vite';
import { openDialog } from './fileDialog';
import { clearDialogAnswers, dialogState, queueDialogAnswers } from '../qa/dialogAnswers';

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));

afterEach(() => {
  vi.unstubAllEnvs();
  vi.clearAllMocks();
  clearDialogAnswers();
});

it('shows the native dialog in ordinary builds, even with answers queued', async () => {
  vi.mocked(open).mockResolvedValue('/picked/by/user.NEF');
  queueDialogAnswers(['/queued.NEF']);
  expect(await openDialog({ title: 'Locate' })).toBe('/picked/by/user.NEF');
  expect(open).toHaveBeenCalledWith({ title: 'Locate' });
  expect(dialogState().pending).toBe(1);
});

it('returns queued answers in QA builds, then falls back to the native dialog', async () => {
  vi.stubEnv('VITE_RAWTONIC_QA', '1');
  vi.mocked(open).mockResolvedValue('/native.NEF');
  queueDialogAnswers(['/Volumes/QA/A.NEF', null]);
  expect(await openDialog({ title: 'Locate A', defaultPath: '/old', directory: false })).toBe('/Volumes/QA/A.NEF');
  expect(await openDialog({ title: 'Locate folder', directory: true })).toBeNull();
  expect(await openDialog({ title: 'Third' })).toBe('/native.NEF');
  expect(open).toHaveBeenCalledTimes(1);
  expect(dialogState()).toEqual({
    pending: 0,
    asked: [
      { title: 'Locate A', defaultPath: '/old', directory: false, multiple: false, answer: '/Volumes/QA/A.NEF' },
      { title: 'Locate folder', defaultPath: null, directory: true, multiple: false, answer: null },
      { title: 'Third', defaultPath: null, directory: false, multiple: false, answer: 'native' },
    ],
  });
});

/** Builds fileDialog.ts as production does and returns every emitted chunk's code. */
async function bundle(qa: boolean): Promise<string> {
  const file = (relative: string) => decodeURIComponent(new URL(relative, import.meta.url).pathname);
  // Vite reads VITE_* variables from the environment, as `tauri build` passes them.
  vi.stubEnv('VITE_RAWTONIC_QA', qa ? '1' : undefined);
  try {
    const output = await build({
      root: file('../..'),
      configFile: false,
      logLevel: 'silent',
      mode: 'production',
      build: {
        write: false,
        minify: false,
        lib: { entry: file('./fileDialog.ts'), formats: ['es'] },
        rollupOptions: { external: [/^@tauri-apps\//] },
      },
    });
    const results = Array.isArray(output) ? output : [output];
    return results
      .flatMap((result) => ('output' in result ? result.output : []))
      .map((chunk) => ('code' in chunk ? chunk.code : ''))
      .join('\n');
  } finally {
    vi.unstubAllEnvs();
  }
}

it('compiles the QA answers out of ordinary builds', async () => {
  const ordinary = await bundle(false);
  expect(ordinary).toContain('plugin-dialog');
  expect(ordinary).not.toContain('takeDialogAnswer');
  expect(ordinary).not.toContain('dialogAnswers');
  const qa = await bundle(true);
  expect(qa).toContain('takeDialogAnswer');
}, 60_000);
