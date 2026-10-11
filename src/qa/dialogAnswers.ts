// QA builds only (VITE_RAWTONIC_QA=1): answers for the next native open
// dialogs, queued over the QA control channel (`dialog.answer`) and taken by
// src/utils/fileDialog.ts. Ordinary builds never import this module.
import type { OpenDialogOptions } from '@tauri-apps/plugin-dialog';

/** A queued answer: a path, several paths, or null (the user cancelled). */
export type DialogAnswer = string | string[] | null;

export interface AskedDialog {
  title: string | null;
  defaultPath: string | null;
  directory: boolean;
  multiple: boolean;
  /** What the dialog returned: from the queue, or `native` when it showed the panel. */
  answer: DialogAnswer | 'native';
}

const queue: DialogAnswer[] = [];
const asked: AskedDialog[] = [];

/** Queues answers for the next dialogs, in order. */
export function queueDialogAnswers(answers: DialogAnswer[]) {
  queue.push(...answers);
  return { pending: queue.length };
}

/** The next queued answer for a dialog asked with `options`; undefined when
 * none is queued (the native panel opens then). Records what was asked. */
export function takeDialogAnswer(options: OpenDialogOptions): DialogAnswer | undefined {
  const answer = queue.length ? queue.shift() : undefined;
  asked.push({
    title: options.title ?? null,
    defaultPath: options.defaultPath ?? null,
    directory: Boolean(options.directory),
    multiple: Boolean(options.multiple),
    answer: answer === undefined ? 'native' : answer,
  });
  if (asked.length > 50) asked.shift();
  return answer;
}

/** The dialogs asked so far (most recent last) and the answers still queued. */
export function dialogState() {
  return { pending: queue.length, asked: [...asked] };
}

/** Drops queued answers and the record. */
export function clearDialogAnswers() {
  queue.length = 0;
  asked.length = 0;
  return { pending: 0 };
}
