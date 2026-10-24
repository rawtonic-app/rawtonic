// SPDX-License-Identifier: AGPL-3.0-or-later
// Anonymous usage statistics (ADR 0017, channel 4): which tools from a
// fixed list were used this week (yes/no), noted only while the user has
// switched statistics on (Preferences, default off). The app keeps the
// week's counters (crates/contribute/src/stats.rs) and sends each finished
// week once at a later start; "Show what is sent" shows the exact body.
import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../ui/AppProperties";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { TFunction } from "i18next";
import type { Adjustments } from "../../utils/adjustments";

/** The relay's tool keys (services/relay/src/schema.ts `TOOLS`). */
export const STATS_TOOLS = [
  "basic",
  "curves",
  "color",
  "colorGrading",
  "sharpen",
  "noiseReduction",
  "aiDenoise",
  "lensCorrection",
  "transform",
  "crop",
  "masks",
  "aiMasks",
  "retouch",
  "redEye",
  "relight",
  "generative",
  "presets",
  "copySettings",
  "snapshots",
  "merge",
  "export",
  "batchExport",
  "tether",
  "ingest",
  "search",
  "culling",
] as const;
export type StatsTool = (typeof STATS_TOOLS)[number];

/** Which tool a changed recipe key belongs to. Keys of other tools (effects,
 * lens blur, view state) and the version keys that travel with a change
 * count for none. */
export const KEY_TOOLS: Record<string, StatsTool> = {};
const keysOf = (tool: StatsTool, keys: string[]) => keys.forEach((key) => (KEY_TOOLS[key] = tool));
keysOf("basic", [
  "exposure",
  "toneMapper",
  "baseCurve",
  "cameraProfile",
  "brightness",
  "contrast",
  "highlights",
  "shadows",
  "whites",
  "blacks",
  "temperature",
  "tint",
  "whiteBalance",
  "wbTemperature",
  "wbTint",
  "saturation",
  "vibrance",
]);
keysOf("curves", ["curves", "pointCurves", "parametricCurve", "curveMode", "levels", "regionCurve"]);
keysOf("color", ["hue", "hsl", "colorRanges", "colorCalibration", "blackAndWhite", "grayMixer"]);
keysOf("colorGrading", ["colorGrading"]);
keysOf("sharpen", ["sharpness", "sharpnessThreshold", "sharpnessFalloff", "sharpenRadius", "sharpenDetail", "sharpenMasking"]);
keysOf("noiseReduction", [
  "lumaNoiseReduction",
  "colorNoiseReduction",
  "lumaNoiseDetail",
  "lumaNoiseContrast",
  "colorNoiseDetail",
  "colorNoiseSmoothness",
]);
keysOf("aiDenoise", ["aiDenoise"]);
keysOf("lensCorrection", [
  "lensCorrectionMode",
  "lensMaker",
  "lensModel",
  "lensDistortionAmount",
  "lensVignetteAmount",
  "lensTcaAmount",
  "lensDistortionEnabled",
  "lensTcaEnabled",
  "lensVignetteEnabled",
  "diffractionCorrection",
  "autoChromaticAberration",
  "chromaticAberrationRedCyan",
  "chromaticAberrationBlueYellow",
  "defringePurpleAmount",
  "defringePurpleHueLo",
  "defringePurpleHueHi",
  "defringeGreenAmount",
  "defringeGreenHueLo",
  "defringeGreenHueHi",
]);
keysOf("transform", [
  "rotation",
  "flipHorizontal",
  "flipVertical",
  "orientationSteps",
  "transformDistortion",
  "transformVertical",
  "transformHorizontal",
  "transformRotate",
  "transformAspect",
  "transformScale",
  "transformXOffset",
  "transformYOffset",
  "guidedPerspective",
]);
keysOf("crop", ["crop", "aspectRatio"]);
keysOf("retouch", ["spots", "faceRetouch"]);
keysOf("redEye", ["redEyes"]);
keysOf("relight", ["relightLights", "relightDepthAmount", "relightSkinReflections", "relightOriginalLight"]);
keysOf("generative", ["aiPatches"]);

const RETOUCH_MASKS = new Set(["clone", "heal", "liquify", "retouch", "quick-eraser"]);

const same = (a: unknown, b: unknown) => a === b || JSON.stringify(a) === JSON.stringify(b);

type MaskLayer = { id?: string; subMasks?: { type?: string }[] };
const layersOf = (masks: unknown): MaskLayer[] => (Array.isArray(masks) ? (masks as MaskLayer[]) : []);

/** The mask kinds of the layers an edit added or changed (not of layers it
 * left as they were). */
const changedMaskTypes = (before: unknown, after: unknown): string[] => {
  const old = new Map(layersOf(before).map((layer, i) => [layer.id ?? `#${i}`, layer]));
  return layersOf(after)
    .filter((layer, i) => !same(old.get(layer.id ?? `#${i}`), layer))
    .flatMap((layer) => (layer.subMasks ?? []).map((s) => s.type ?? ""));
};

/** The tools an edit step used: the tools of the keys it changed; for masks
 * the kinds of mask the layers it added or changed hold (AI masks,
 * retouching, the others; removing a layer counts as masks). */
export const editedTools = (before: Partial<Adjustments>, after: Partial<Adjustments>): StatsTool[] => {
  const used = new Set<StatsTool>();
  const keys = new Set([...Object.keys(before), ...Object.keys(after)]);
  const b = before as Record<string, unknown>;
  const a = after as Record<string, unknown>;
  for (const key of keys) {
    if (same(b[key], a[key])) continue;
    if (key === "masks") {
      const changed = changedMaskTypes(b.masks, a.masks);
      if (changed.some((type) => type.startsWith("ai-"))) used.add("aiMasks");
      if (changed.some((type) => RETOUCH_MASKS.has(type))) used.add("retouch");
      if (changed.some((type) => !type.startsWith("ai-") && !RETOUCH_MASKS.has(type)) || changed.length === 0)
        used.add("masks");
      continue;
    }
    const tool = KEY_TOOLS[key];
    if (tool) used.add(tool);
  }
  return [...used];
};

const statsOn = () => useSettingsStore.getState().appSettings?.usageStats === true;

/** Notes tools as used this week; nothing at all while statistics are off. */
export const noteTools = (tools: StatsTool[]) => {
  if (!statsOn() || tools.length === 0) return;
  void invoke(Invokes.UsageStatsNoteTools, { tools }).catch(() => undefined);
};

export const noteTool = (tool: StatsTool) => noteTools([tool]);

/** An edit step of the open photo (useEditorStore `pushHistory`). */
export const noteEdit = (before: Partial<Adjustments> | undefined, after: Partial<Adjustments>) => {
  if (!statsOn() || !before) return;
  noteTools(editedTools(before, after));
};

export interface UsageStatsPreview {
  /** Every body kept to be sent, exactly as it will be, in send order:
   * finished weeks (sent at the next start), then this week so far. */
  pending?: { week: string; body: string; current: boolean }[];
  lastSent?: { week: string; body: string } | null;
}

export const usageStatsPreview = () => invoke<UsageStatsPreview>(Invokes.UsageStatsPreview);
export const setUsageStatsEnabled = (enabled: boolean) => invoke<void>(Invokes.UsageStatsSetEnabled, { enabled });

interface StatsBody {
  week?: string;
  app?: { version?: string; os?: string; osVersion?: string; arch?: string };
  gpuVendor?: string;
  cameras?: { make?: string; model?: string }[];
  tools?: Record<string, boolean>;
}

/** A body in plain words: one line each for the week, the system, the
 * cameras and the tools (`toolName` names a tool key). */
export const plainLines = (
  body: string,
  t: TFunction,
  toolName: (tool: string) => string,
): string[] => {
  let p: StatsBody;
  try {
    p = JSON.parse(body) as StatsBody;
  } catch {
    return [];
  }
  const os = { macos: "macOS", windows: "Windows", linux: "Linux" }[p.app?.os ?? ""] ?? p.app?.os ?? "";
  const gpu = { apple: "Apple", amd: "AMD", intel: "Intel", nvidia: "NVIDIA", qualcomm: "Qualcomm" }[p.gpuVendor ?? ""];
  const cameras = (p.cameras ?? []).map((c) => `${c.make ?? ""} ${c.model ?? ""}`.trim());
  const tools = Object.keys(p.tools ?? {}).map(toolName);
  return [
    t("contribute.stats.lines.week", { week: p.week ?? "?" }),
    t("contribute.stats.lines.system", {
      version: p.app?.version ?? "?",
      os: `${os} ${p.app?.osVersion ?? ""}`.trim(),
      arch: p.app?.arch ?? "?",
      gpu: gpu ?? t("contribute.stats.otherGpu"),
    }),
    t("contribute.stats.lines.cameras", {
      cameras: cameras.length ? cameras.join(", ") : t("contribute.stats.none"),
    }),
    t("contribute.stats.lines.tools", { tools: tools.length ? tools.join(", ") : t("contribute.stats.none") }),
  ];
};
