/** The job name `{job}` fills in export names and folders (Capture One's
 * job name): set once in the Export dialog and remembered per library, so
 * every export of a shoot, from the dialog or the File menu, uses it. A
 * library without one uses its folder's name, as `{job}` on import holds
 * the library's name. */
const key = (library: string | null) => `rawtonic.export.job:${library ?? ""}`;
// A catalog's folder carries ".catalog" after its name (`SessionWorkspace`).
const libraryName = (library: string | null) =>
  ((library ?? "").replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? "").replace(/\.catalog$/i, "");

export function readJobName(library: string | null): string {
  try {
    const saved = localStorage.getItem(key(library));
    if (saved !== null) return saved;
  } catch {
    // Storage unavailable: the library's name still names the job.
  }
  return libraryName(library);
}

export function writeJobName(library: string | null, name: string): void {
  try {
    localStorage.setItem(key(library), name);
  } catch {
    // Kept for this export only.
  }
}
