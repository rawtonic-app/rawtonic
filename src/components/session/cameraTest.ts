import compatibility from "./tetherCameras.json";

/** How well tethering works with a camera (tetherCameras.json). */
export type SupportLevel = "tested" | "reported" | "notSupported";

export interface CompatibilityEntry {
  name: string;
  /** The camera library's model names. */
  match?: string[];
  /** A start of them (a whole maker). */
  matchPrefix?: string;
  level: SupportLevel;
  usbMode?: string;
  tested?: string;
  systems?: string[];
  photoOnScreen?: { median: number; p95: number };
  shots?: string;
  transferQueue?: boolean;
  works: string[];
  notes?: string;
}

export const COMPATIBILITY = compatibility.cameras as CompatibilityEntry[];

/** The list's entry for a camera model, if it has one. */
export const compatibilityOf = (model: string, list: CompatibilityEntry[] = COMPATIBILITY) =>
  list.find((entry) => entry.match?.includes(model)) ??
  list.find((entry) => entry.matchPrefix !== undefined && model.startsWith(entry.matchPrefix));

export type StepId =
  | "connect"
  | "settings"
  | "setSetting"
  | "autofocus"
  | "shots"
  | "burst"
  | "bodyShot"
  | "replug"
  | "release";

/** The guided test's steps, in order. */
export const STEPS: StepId[] = [
  "connect",
  "settings",
  "setSetting",
  "autofocus",
  "shots",
  "burst",
  "bodyShot",
  "replug",
  "release",
];

export type StepState = "passed" | "failed" | "skipped";

/** A finished step: what it measured (numbers in ms unless named
 * otherwise). The report takes only values the test made itself, never a
 * path, file name or serial number. */
export interface StepResult {
  state: StepState;
  /** Why it failed or was skipped, in the test's own words (reported). */
  reason?: string;
  /** The camera's or the app's message: shown in the test only, never in
   * the report (it may name folders, files or the user). */
  detail?: string;
  ms?: number;
  /** Photos asked for / arrived in the library. */
  asked?: number;
  arrived?: number;
  /** Request → photo verified in the library. */
  inLibrary?: number[];
  /** Request → first Develop frame of the photo rendered. */
  onScreen?: number[];
  /** Settings read / changeable. */
  settings?: number;
  changeable?: number;
  /** The setting changed (its label). */
  setting?: string;
  /** Photos freed from the camera's transfer queue after their commit. */
  freed?: number;
}

/** The middle value (the upper one of an even count), or null. */
export const median = (values: number[]) => percentile(values, 50);

/** Nearest-rank percentile, or null for no values. */
export const percentile = (values: number[], p: number) => {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const rank = Math.ceil((p / 100) * sorted.length);
  return sorted[Math.min(sorted.length, Math.max(1, rank)) - 1];
};

export interface ReportInfo {
  appVersion: string;
  /** e.g. "macos 26.0.1 (aarch64)". */
  system: string;
  model: string;
  firmware: string | null;
  /** libgphoto2's version, null for the mock camera. */
  library: string | null;
  transport: string;
  transferQueue: boolean;
  freeCameraCopies: boolean;
  /** Files of the test's photos that did not go into the library. */
  filesNotAdded: number;
}

const TRANSPORTS: Record<string, string> = {
  imageCaptureCore: "ImageCaptureCore (macOS camera service)",
  usb: "libusb",
  mock: "mock camera (no camera library)",
};

const LEVELS: Record<SupportLevel, string> = {
  tested: "tested",
  reported: "reported working",
  notSupported: "not supported",
};

const STEP_NAMES: Record<StepId, string> = {
  connect: "Connect",
  settings: "Read settings",
  setSetting: "Change a setting",
  autofocus: "Autofocus",
  shots: "Shots from Rawtonic",
  burst: "Burst from Rawtonic",
  bodyShot: "Shot on the camera",
  replug: "Cable out and back in",
  release: "Transfer queue freed after commit",
};

const timings = (name: string, values: number[] | undefined) => {
  if (!values?.length) return null;
  return `${name} median ${median(values)} ms, p95 ${percentile(values, 95)} ms (n=${values.length})`;
};

/** What a step measured, in one line. */
export const describe = (result: StepResult) => {
  const parts: string[] = [];
  if (result.asked !== undefined) parts.push(`${result.arrived ?? 0}/${result.asked} in the library`);
  if (result.settings !== undefined) parts.push(`${result.settings} read, ${result.changeable ?? 0} changeable`);
  if (result.setting) parts.push(`${result.setting} changed, read back and set back`);
  if (result.freed !== undefined) parts.push(`${result.freed} freed`);
  const screen = timings("photo on screen", result.onScreen);
  if (screen) parts.push(screen);
  const library = timings("in library", result.inLibrary);
  if (library) parts.push(library);
  if (result.ms !== undefined) parts.push(`${result.ms} ms`);
  if (result.reason) parts.push(result.reason);
  return parts.join("; ");
};

/** The anonymous report the user copies (Markdown, for the issue
 * template): the app, the system, the camera model and firmware, the
 * driver path and each step's result. Nothing else — no serial number,
 * path, user name, file name or photo. */
export const buildReport = (info: ReportInfo, results: Partial<Record<StepId, StepResult>>) => {
  const entry = compatibilityOf(info.model);
  const driver = info.library
    ? `libgphoto2 ${info.library} via ${TRANSPORTS[info.transport] ?? info.transport}`
    : (TRANSPORTS[info.transport] ?? info.transport);
  const lines = [
    "### Rawtonic camera test",
    "",
    `- Rawtonic: ${info.appVersion}`,
    `- System: ${info.system}`,
    `- Camera: ${info.model}`,
    `- Firmware: ${info.firmware ?? "not reported"}`,
    `- Camera driver: ${driver}`,
    `- Transfer queue: ${info.transferQueue ? `yes (freeing ${info.freeCameraCopies ? "on" : "off"})` : "no"}`,
    `- Compatibility list: ${entry ? LEVELS[entry.level] : "not listed"}`,
    `- Files not added to the library: ${info.filesNotAdded}`,
    "",
    "| Step | Result | Measured |",
    "|---|---|---|",
  ];
  for (const id of STEPS) {
    const result = results[id];
    const state = result ? { passed: "pass", failed: "FAIL", skipped: "skipped" }[result.state] : "not run";
    const measured = result ? describe(result).replace(/\|/g, "/") : "";
    lines.push(`| ${STEP_NAMES[id]} | ${state} | ${measured} |`);
  }
  lines.push(
    "",
    "Photo on screen: from the test's shutter request to the first Develop frame of the new photo rendered " +
      "(the display's own delay not included). In library: from the request to the photo verified in the library.",
  );
  return lines.join("\n");
};

/** The setting the test changes and sets back: the first changeable one of
 * these with another choice (never Save to or the image quality). */
const SETTING_ORDER = ["iso", "whitebalance", "exposurecompensation", "shutterspeed", "aperture"];

export const settingToChange = <S extends { key: string; value: string; choices: string[]; readOnly: boolean }>(
  settings: S[],
) => {
  for (const key of SETTING_ORDER) {
    const setting = settings.find((s) => s.key === key && !s.readOnly && s.choices.length > 1);
    if (!setting) continue;
    const at = setting.choices.indexOf(setting.value);
    const other = setting.choices[at >= 0 && at + 1 < setting.choices.length ? at + 1 : 0];
    if (other !== setting.value) return { setting, other };
    return { setting, other: setting.choices[1] };
  }
  return null;
};
