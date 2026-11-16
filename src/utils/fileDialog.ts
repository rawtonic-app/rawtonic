import { open, save, type OpenDialogOptions, type SaveDialogOptions } from '@tauri-apps/plugin-dialog';
import { homeDir, pictureDir } from '@tauri-apps/api/path';
import { answerFolder, isBareName, joinFolder, rememberDialogFolder, rememberedDialogFolder } from './dialogFolders';

/**
 * Every native dialog says what is being chosen (its title) and may name a
 * purpose: it then opens where the last dialog for that purpose was
 * answered (UXR4L-01). On Linux the dialogs go through the desktop portal
 * (src-tauri/src/startup.rs `use_portal_dialogs`).
 */
interface DialogIntent {
  /** What is chosen, said in the dialog's title bar ("Choose where the new library is created"). */
  title: string;
  /** Dialogs of one purpose share their start folder ("import", "export", "presets"…). */
  purpose?: string;
}
export type OpenFileDialogOptions = Omit<OpenDialogOptions, 'title'> & DialogIntent;
export type SaveFileDialogOptions = Omit<SaveDialogOptions, 'title'> & DialogIntent;

/** The start folder when nothing was remembered: Pictures, else home. */
async function fallbackFolder(): Promise<string | undefined> {
  const candidates: Array<() => Promise<string>> = [() => pictureDir(), () => homeDir()];
  for (const folder of candidates) {
    try {
      const path = await folder();
      if (path) return path;
    } catch {
      // Not every system has a Pictures folder.
    }
  }
  return undefined;
}

/** The caller's start path, else the remembered folder, else Pictures. A
 * save dialog's bare file name goes into that folder. */
async function startPath(defaultPath: string | undefined, purpose: string | undefined): Promise<string | undefined> {
  if (defaultPath && !isBareName(defaultPath)) return defaultPath;
  const folder = rememberedDialogFolder(purpose) ?? (await fallbackFolder());
  if (!folder) return defaultPath;
  return defaultPath ? joinFolder(folder, defaultPath) : folder;
}

/**
 * The native open dialog, with a seam for QA builds: the QA control channel
 * (`dialog.answer`, src/qa/qaControl.ts) can queue the paths the next dialogs
 * return, because a native panel cannot be driven from the channel. Ordinary
 * builds compile the branch out (`VITE_RAWTONIC_QA` is not `'1'`), so they
 * always show the native panel.
 */
export async function openDialog({ purpose, ...options }: OpenFileDialogOptions): Promise<string | string[] | null> {
  const asked: OpenDialogOptions = { ...options, defaultPath: await startPath(options.defaultPath, purpose) };
  if (asked.defaultPath === undefined) delete asked.defaultPath;
  let answer: string | string[] | null | undefined;
  if (import.meta.env.VITE_RAWTONIC_QA === '1') {
    const { takeDialogAnswer } = await import('../qa/dialogAnswers');
    answer = takeDialogAnswer(asked);
  }
  if (answer === undefined) answer = await open(asked);
  const first = Array.isArray(answer) ? answer[0] : answer;
  if (first) rememberDialogFolder(purpose, answerFolder(first, Boolean(options.directory)));
  return answer;
}

/** The native save dialog, with the same QA seam: `dialog.answer` queues the file path it returns. */
export async function saveDialog({ purpose, ...options }: SaveFileDialogOptions): Promise<string | null> {
  const asked: SaveDialogOptions = { ...options, defaultPath: await startPath(options.defaultPath, purpose) };
  if (asked.defaultPath === undefined) delete asked.defaultPath;
  let answer: string | null | undefined;
  if (import.meta.env.VITE_RAWTONIC_QA === '1') {
    const { takeDialogAnswer } = await import('../qa/dialogAnswers');
    const queued = takeDialogAnswer(asked);
    if (queued !== undefined) answer = typeof queued === 'string' ? queued : null;
  }
  if (answer === undefined) answer = await save(asked);
  if (answer) rememberDialogFolder(purpose, answerFolder(answer, false));
  return answer;
}
