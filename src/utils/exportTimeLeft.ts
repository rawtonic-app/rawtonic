import type { ExportItem } from '../store/useProcessStore';

/**
 * Milliseconds a running export job still needs, or null until a photo of
 * it has finished. Each photo is assumed to take as long as the job's
 * finished photos took on average; the photos in progress share the
 * workers, so the photos not started yet are spread over that many.
 */
export function exportTimeLeft(items: ExportItem[], now: number): number | null {
  const durations = items
    .filter((item) => item.state === 'done' && item.startedMs != null && item.finishedMs != null)
    .map((item) => (item.finishedMs as number) - (item.startedMs as number));
  if (!durations.length) return null;
  const average = durations.reduce((sum, d) => sum + d, 0) / durations.length;
  const inProgress = items
    .filter((item) => item.state === 'running')
    .map((item) => Math.max(0, average - (now - (item.startedMs ?? now))));
  const notStarted = items.filter((item) => item.state === 'pending').length;
  if (!notStarted) return inProgress.length ? Math.max(...inProgress) : 0;
  const workers = Math.max(1, inProgress.length);
  return (inProgress.reduce((sum, left) => sum + left, 0) + notStarted * average) / workers;
}

/** Whole seconds, minutes or hours and minutes, for "… left". */
export function splitTimeLeft(ms: number): { hours: number; minutes: number; seconds: number } {
  // Rounded up to 5 s so the last seconds do not read "0 s".
  const total = Math.max(5, Math.ceil(ms / 5000) * 5);
  if (total < 60) return { hours: 0, minutes: 0, seconds: total };
  const minutes = Math.ceil(total / 60);
  return { hours: Math.floor(minutes / 60), minutes: minutes % 60, seconds: 0 };
}
