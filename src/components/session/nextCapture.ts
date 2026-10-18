import { withAiDenoiseVersion, withGrainStructure } from "../../utils/adjustments";

/** Mirrors `hot_folder::NextCapture`. */
export type NextCapture =
  | { mode: "defaults" }
  | { mode: "copyFromLast" }
  | { mode: "preset"; name: string; adjustments: Record<string, unknown> };

/** Mirrors `hot_folder::NextCaptureMetadata`. */
export interface NextCaptureMetadata {
  rating: number;
  keywords: string[];
}

/** What new captures (hot folder, tether) start with, as the form holds
 * it: `choice` is "defaults", "copyFromLast" or "preset:<id>". */
export interface NextCaptureForm {
  choice: string;
  rating: number;
  keywords: string;
}

export const DEFAULT_NEXT_CAPTURE: NextCaptureForm = { choice: "defaults", rating: 0, keywords: "" };

export interface PresetChoice {
  id: string;
  name: string;
  adjustments: Record<string, unknown>;
}

/** Presets of `load_presets`, folders flattened. */
export type PresetItemJson =
  | { preset: PresetChoice }
  | { folder: { name: string; children: PresetChoice[] } };
export function flattenPresets(items: PresetItemJson[]): PresetChoice[] {
  return items.flatMap((item) =>
    "preset" in item
      ? [item.preset]
      : item.folder.children.map((p) => ({ ...p, name: `${item.folder.name} / ${p.name}` })),
  );
}

/** Next-capture keywords typed as a comma-separated list. */
export function parseKeywords(text: string): string[] {
  const keywords: string[] = [];
  for (const part of text.split(/[,;]/)) {
    const keyword = part.trim();
    if (keyword && !keywords.includes(keyword)) keywords.push(keyword);
  }
  return keywords;
}

/** The form as the backend takes it. */
export function nextCaptureOf(
  form: NextCaptureForm,
  presets: PresetChoice[],
): { nextCapture: NextCapture; metadata: NextCaptureMetadata } {
  const preset = presets.find((p) => `preset:${p.id}` === form.choice);
  const nextCapture: NextCapture = preset
    ? { mode: "preset", name: preset.name, adjustments: withAiDenoiseVersion(withGrainStructure(preset.adjustments)) }
    : form.choice === "copyFromLast"
      ? { mode: "copyFromLast" }
      : { mode: "defaults" };
  return { nextCapture, metadata: { rating: form.rating, keywords: parseKeywords(form.keywords) } };
}
