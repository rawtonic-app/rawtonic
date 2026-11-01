import { DEFAULT_RECIPE_DRAFT, type RecipeDraft } from './exportRecipes';

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

/** The Export form's last settings, remembered per library like the job
 * name, so reopening the dialog starts where the last export left off. Only
 * keys the form knows are taken back, so a stale entry cannot add fields. */
const formKey = (library: string | null) => `rawtonic.export.form:${library ?? ""}`;

export function readFormDraft(library: string | null): RecipeDraft | null {
  try {
    const saved = JSON.parse(localStorage.getItem(formKey(library)) ?? 'null');
    if (!saved || typeof saved !== 'object' || Array.isArray(saved)) return null;
    const draft: Record<string, unknown> = { ...DEFAULT_RECIPE_DRAFT };
    for (const [name, fallback] of Object.entries(DEFAULT_RECIPE_DRAFT)) {
      if (name in saved && typeof saved[name] === typeof fallback) draft[name] = saved[name];
    }
    if (typeof saved.folder === 'string') draft.folder = saved.folder;
    return draft as unknown as RecipeDraft;
  } catch {
    return null;
  }
}

export function writeFormDraft(library: string | null, draft: RecipeDraft): void {
  try {
    localStorage.setItem(formKey(library), JSON.stringify(draft));
  } catch {
    // Kept for this dialog only.
  }
}
