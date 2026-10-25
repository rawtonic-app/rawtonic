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
import { COPIED_ALONG } from "./StudioToolCopy";

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

/** Keys that only say how a recipe reads its values (they go along with
 * the values, never make a preset one tool's). */
const ALONG = new Set([...Object.values(COPIED_ALONG).flat(), "grainVersion", "diffractionVersion"]);

/** Whether a preset sets this tool's parameters and nothing else. */
export function presetFitsTool(preset: Preset, toolKeys: readonly string[]) {
  const keys = Object.keys(preset.adjustments ?? {}).filter((key) => !ALONG.has(key));
  return keys.length > 0 && keys.every((key) => toolKeys.includes(key));
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
