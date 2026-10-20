import { invoke } from '@tauri-apps/api/core';

/** The hot folder commands the app sends (`hot_folder.rs`). */
export type HotFolderCommand = 'hot_folder_start' | 'hot_folder_resume' | 'hot_folder_suspend' | 'hot_folder_stop';

let last: Promise<unknown> = Promise.resolve();

/** Sends a hot folder command once the ones sent before it have finished:
 * the backend runs them in blocking workers, which do not keep their order,
 * and a suspend of one library must never run after the next resume of it
 * (an A → B → A switch would stop the watch the app just showed). */
export function hotFolderCommand<T>(command: HotFolderCommand, args: Record<string, unknown>): Promise<T> {
  const run = last.then(
    () => invoke<T>(command, args),
    () => invoke<T>(command, args),
  );
  last = run.catch(() => undefined);
  return run;
}
