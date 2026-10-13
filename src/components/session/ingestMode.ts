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
