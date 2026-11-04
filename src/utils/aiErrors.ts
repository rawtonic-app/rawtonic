import i18n from 'i18next';
import { notify } from '../activity/notify';
import { errorCode, errorMessage } from '../shared/ipc/errors';

/** One toast for the runtime, however many AI tools fail at once. */
const RUNTIME_TOAST_ID = 'ai-runtime-unavailable';

/** Background work (keywords, masks computed ahead) reports the runtime once
 * per session. */
let backgroundRuntimeNoticeShown = false;

/** ONNX Runtime is missing or cannot be loaded (`rawtonic_onnx::RuntimeError`). */
export function isAiRuntimeError(error: unknown): boolean {
  return errorCode(error) === 'ai_runtime_unavailable';
}

/** The backend had another photo loaded than the mask was asked for
 * (`ai_commands::PHOTO_CHANGED`): the request is dropped, not failed. */
export function isPhotoChanged(error: unknown): boolean {
  return errorCode(error) === 'stale';
}

/** The task was cancelled (Cancel, or a newer request replaced it). */
export function isAiTaskCancelled(error: unknown): boolean {
  return errorCode(error) === 'cancelled';
}

/** Text for an AI tool's failure: our own translated sentence when the AI
 * runtime is missing or broken, else `fallback` (the error by default). */
export function aiErrorMessage(error: unknown, fallback?: string): string {
  if (isAiRuntimeError(error)) return i18n.t('editor.ai.runtimeUnavailable');
  return fallback ?? errorMessage(error);
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
