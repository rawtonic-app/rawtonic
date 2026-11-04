/**
 * Errors of Tauri commands. A converted command rejects with
 * `{ code, message }` (`src-tauri/src/ipc/error.rs`); the others still
 * reject with a plain string. Branch on `errorCode`, never on the text;
 * show `errorMessage`, which is the text the command gave before.
 */

/** The codes of `ipc::ErrorCode` (snake_case, as serialized). */
export type ErrorCode =
  | 'cancelled'
  | 'not_found'
  | 'already_exists'
  | 'permission_denied'
  | 'unsupported'
  | 'invalid'
  | 'busy'
  | 'internal'
  | 'stale'
  | 'ai_runtime_unavailable'
  | 'models_missing'
  | 'mask_parameters_missing'
  | 'live_stroke_resync'
  | 'render_failed'
  | 'generated_areas'
  | 'exports_pending';

export interface CommandError {
  code: ErrorCode;
  message: string;
}

export function isCommandError(error: unknown): error is CommandError {
  return (
    typeof error === 'object' &&
    error !== null &&
    typeof (error as { code?: unknown }).code === 'string' &&
    typeof (error as { message?: unknown }).message === 'string'
  );
}

/** The code of a converted command's error; `undefined` for anything else
 * (a plain string from a command not converted yet, a JavaScript error). */
export function errorCode(error: unknown): ErrorCode | undefined {
  return isCommandError(error) ? error.code : undefined;
}

/** The text to show for any rejection: a string as it is, the `message`
 * of a command error (or another object with one, such as an `Error` or a
 * `SaveFailure`), else `String(error)`. */
export function errorMessage(error: unknown): string {
  if (typeof error === 'string') return error;
  if (typeof error === 'object' && error !== null) {
    const { message } = error as { message?: unknown };
    if (typeof message === 'string') return message;
  }
  return String(error);
}
