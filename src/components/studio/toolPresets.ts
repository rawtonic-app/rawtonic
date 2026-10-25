import type { Preset } from "../ui/AppProperties";
import {
  withAiDenoiseVersion,
  withDiffractionVersion,
  withGrainStructure,
  withLegacyGrainVersion,
  withLumaNoiseVersion,
  withPresetClarityVersion,
  withSharpenVersion,
  type Adjustments,
} from "../../utils/adjustments";
import { developmentSignature } from "../../utils/aiDenoise";
import { COPIED_ALONG, pick } from "./StudioToolCopy";

/** Tools with Capture One's per-tool preset menu in their header, by tool
 * id (`ToolGroup`'s `title`). A tool opts in by its id; its presets are
 * those whose settings are all among its copy/apply parameters. */
export const PRESET_TOOLS: ReadonlySet<string> = new Set([
  "Grain",
  "Vignette",
  "Color grading",
  "Black & white",
  "Sharpening",
  "Noise reduction",
]);

/** Keys that say how a recipe reads a parameter's value, which go along
 * with it (tool copy's, and the grain version). */
const alongOf = (key: string) => [...(COPIED_ALONG[key] ?? []), ...(key.startsWith("grain") ? ["grainVersion"] : [])];

/** Whether a preset sets this tool's parameters and nothing else: besides
 * them only the keys that go along with them (a grain preset that also
 * names a colour noise filter is not the grain tool's). */
export function presetFitsTool(preset: Preset, toolKeys: readonly string[]) {
  const allowed = new Set(toolKeys.flatMap((key) => [key, ...alongOf(key)]));
  const keys = Object.keys(preset.adjustments ?? {});
  return keys.some((key) => toolKeys.includes(key)) && keys.every((key) => allowed.has(key));
}

/** The photo with a preset for the hover preview, or null when the preset
 * changes how the RAW is developed before any adjustment (AI Denoise, the
 * colour noise filter): the preview re-renders the open source only, so it
 * would show something else than applying the preset does. */
export function presetPreview(current: Adjustments, adjustments: Partial<Adjustments>, isRaw: boolean): Adjustments | null {
  const previewed = { ...current, ...presetValues(adjustments) } as Adjustments;
  return developmentSignature(previewed, isRaw) === developmentSignature(current, isRaw) ? previewed : null;
}

/** A tool's settings to save as its preset: as copied (`pick`), with the
 * colour noise filter of an edit from before its version key stated (it
 * would vanish in JSON and the preset would render with another filter). */
export function toolPresetToSave(current: Adjustments, keys: string[]): Partial<Adjustments> {
  const picked = pick(current, keys);
  if ("colorNoiseVersion" in picked && picked.colorNoiseVersion == null && picked.colorNoiseReduction !== undefined)
    picked.colorNoiseVersion = 1;
  return picked;
}

/** A preset's settings as applying it writes them (the versions that say
 * how its values render, filled in for presets from before them), for the
 * hover preview. */
export const presetValues = (adjustments: Partial<Adjustments>): Partial<Adjustments> =>
  withPresetClarityVersion(
    withDiffractionVersion(
      withLumaNoiseVersion(
        withSharpenVersion(withAiDenoiseVersion(withLegacyGrainVersion(withGrainStructure(structuredClone(adjustments))))),
      ),
    ),
  );
