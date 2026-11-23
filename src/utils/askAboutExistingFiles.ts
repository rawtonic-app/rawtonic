import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import type { CollisionSummary } from '../components/ui/ExportImportProperties';
import { useExportCollisionStore } from '../store/useExportCollisionStore';

type Request = { exportSettings: { existingFiles?: string }; isExplicitFilePath?: boolean } & Record<string, unknown>;

/** Settles the recipes set to "If a file exists: Ask" before an export is
 * queued: the backend finds what their file names meet (nothing is rendered,
 * written or created), and when there is something the photographer is asked
 * once for the whole run: Keep both, Skip or Replace if changed. The answer
 * replaces "ask" in those recipes, so the queue only ever sees the three
 * choices. Returns null when the question was cancelled: nothing is queued.
 * A file the photographer just named in Save As is not asked about (the save
 * panel did): it goes as Replace if changed, which replaces their own earlier
 * export, leaves an unchanged one, and numbers the new file beside any other
 * file; Keep both would fail on a name that is taken. */
export async function askAboutExistingFiles<T extends Request>(requests: T[]): Promise<T[] | null> {
  const isAsking = (request: Request) => request.exportSettings.existingFiles === 'ask';
  const named = (request: T) =>
    isAsking(request) && request.isExplicitFilePath
      ? { ...request, exportSettings: { ...request.exportSettings, existingFiles: 'replaceIfChanged' } }
      : request;
  requests = requests.map(named);
  const asking = requests.filter(isAsking);
  if (!asking.length) return requests;
  const summary = await invoke<CollisionSummary>(Invokes.PlanExportCollisions, { requests: asking });
  // Nothing in the way: every choice writes the same files; Keep both is
  // the one that overwrites nothing should a file appear meanwhile.
  const answer = summary.total > 0 ? await useExportCollisionStore.getState().ask(summary) : 'keepBoth';
  if (!answer) return null;
  return requests.map((request) =>
    isAsking(request) ? { ...request, exportSettings: { ...request.exportSettings, existingFiles: answer } } : request
  );
}
