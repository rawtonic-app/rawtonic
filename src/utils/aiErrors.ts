import i18n from 'i18next';
import { notify } from '../activity/notify';

/** Start of the backend's error when ONNX Runtime is missing or cannot be
 * loaded (`rawtonic_onnx::RUNTIME_UNAVAILABLE`). */
export const AI_RUNTIME_UNAVAILABLE = 'AI runtime unavailable';

/** One toast for the runtime, however many AI tools fail at once. */
const RUNTIME_TOAST_ID = 'ai-runtime-unavailable';

/** Background work (keywords, masks computed ahead) reports the runtime once
 * per session. */
let backgroundRuntimeNoticeShown = false;

export function isAiRuntimeError(error: unknown): boolean {
  return String(error).includes(AI_RUNTIME_UNAVAILABLE);
}

/** The backend had another photo loaded than the mask was asked for
 * (`ai_commands::PHOTO_CHANGED`): the request is dropped, not failed. */
export function isPhotoChanged(error: unknown): boolean {
  return String(error).includes('The photo changed while its mask was asked for');
}

export function isAiTaskCancelled(error: unknown): boolean {
  return String(error).includes('cancelled');
}

/** Text for an AI tool's failure: our own translated sentence when the AI
 * runtime is missing or broken, else `fallback` (the error by default). */
export function aiErrorMessage(error: unknown, fallback?: string): string {
  if (isAiRuntimeError(error)) return i18n.t('editor.ai.runtimeUnavailable');
  return fallback ?? String(error);
}

/** Shows an AI tool's failure as a toast; nothing for a cancelled task.
 * `fallback` is the tool's own message for other errors. */
export function toastAiError(error: unknown, fallback: string) {
  if (isAiTaskCancelled(error)) return;
  if (isAiRuntimeError(error)) {
    notify.error(aiErrorMessage(error), { toastId: RUNTIME_TOAST_ID, source: 'ai' });
    return;
  }
  notify.error(fallback, { source: 'ai' });
}

/** Background AI work failed: only a missing or broken runtime is worth a
 * toast, once per session (the user did not ask for this run). */
export function noteBackgroundAiError(error: unknown) {
  if (!isAiRuntimeError(error) || backgroundRuntimeNoticeShown) return;
  backgroundRuntimeNoticeShown = true;
  notify.error(aiErrorMessage(error), { toastId: RUNTIME_TOAST_ID, source: 'ai' });
}

/** Forgets the session's notices (tests). */
export function resetAiErrorNotices() {
  backgroundRuntimeNoticeShown = false;
}
