/** How an ingest brings photographs in (`session_bridge::IngestMode`):
 * verified copies, or (catalogs only) added where they are. There is no
 * "move". */
export type IngestMode = "copy" | "add";

/** Where a source photograph is (`ingest::batch::Medium`). */
export type Medium = "card" | "disk";

export interface IngestModeChoice {
  /** The mode the dialog starts with. */
  mode: IngestMode;
  /** The two-way switch is shown (catalogs; a session always copies). */
  showSwitch: boolean;
  /** "Add where they are" can be chosen (not for photographs on a card). */
  canAdd: boolean;
}

/** The ingest dialog's mode for its sources, recomputed whenever they
 * change (never remembered from the last import):
 *
 * | library | sources                       | mode | add      |
 * |---------|-------------------------------|------|----------|
 * | session | any                           | copy | no switch|
 * | catalog | any on a card (also mixed)    | copy | disabled |
 * | catalog | into a chosen folder (drop, "Import into this folder…") | copy | allowed |
 * | catalog | none yet                      | copy | allowed  |
 * | catalog | all on drives                 | copy | allowed  |
 */
export function preselectIngestMode({
  catalog,
  files,
}: {
  catalog: boolean;
  files: { medium?: Medium }[];
  intoFolder: boolean;
}): IngestModeChoice {
  if (!catalog) return { mode: "copy", showSwitch: false, canAdd: false };
  if (files.some((file) => file.medium === "card")) return { mode: "copy", showSwitch: true, canAdd: false };
  return { mode: "copy", showSwitch: true, canAdd: true };
}

/** What a camera writes beside a RAW (`ingest::batch::COMPANION_EXTENSIONS`). */
const COMPANIONS = new Set(["jpg", "jpeg", "heic", "heif", "hif"]);

/** Whether `paths` hold a RAW+JPEG pair the ingest would import as one
 * photograph (`ingest::batch::pair_sources`: exactly one RAW, by the app's
 * `raw` extensions, and one JPEG or HEIF of one name, letter case aside,
 * in one folder), so the dialog offers to import such JPEGs as
 * photographs of their own. */
export function hasRawJpegPairs(paths: string[], raw: string[]): boolean {
  const raws = new Set(raw.map((e) => e.toLowerCase()));
  const byName = new Map<string, string[]>();
  for (const path of paths) {
    const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
    const name = path.slice(cut + 1);
    const dot = name.lastIndexOf(".");
    if (dot <= 0) continue;
    const key = `${path.slice(0, cut)}\n${name.slice(0, dot).toLowerCase()}`;
    const extensions = byName.get(key) ?? [];
    extensions.push(name.slice(dot + 1).toLowerCase());
    byName.set(key, extensions);
  }
  return [...byName.values()].some(
    (extensions) =>
      extensions.length === 2 &&
      extensions.some((e) => raws.has(e)) &&
      extensions.some((e) => COMPANIONS.has(e)),
  );
}
