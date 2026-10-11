import { open, type OpenDialogOptions } from '@tauri-apps/plugin-dialog';

/**
 * The native open dialog, with a seam for QA builds: the QA control channel
 * (`dialog.answer`, src/qa/qaControl.ts) can queue the paths the next dialogs
 * return, because a native panel cannot be driven from the channel. Ordinary
 * builds compile the branch out (`VITE_RAWTONIC_QA` is not `'1'`), so they
 * always show the native panel.
 */
export async function openDialog(options: OpenDialogOptions): Promise<string | string[] | null> {
  if (import.meta.env.VITE_RAWTONIC_QA === '1') {
    const { takeDialogAnswer } = await import('../qa/dialogAnswers');
    const answer = takeDialogAnswer(options);
    if (answer !== undefined) return answer;
  }
  return open(options);
}
