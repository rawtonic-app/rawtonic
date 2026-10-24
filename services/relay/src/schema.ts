// SPDX-License-Identifier: AGPL-3.0-or-later
// Payload schemas, version 1 (ADR 0017). Unknown fields are refused; every
// string has a length cap and a character rule.

export class Invalid extends Error {
  constructor(readonly field: string) {
    super(`invalid field: ${field}`);
  }
}

export interface App {
  version: string;
  os: "macos" | "windows" | "linux";
  osVersion: string;
  arch: "aarch64" | "x86_64";
}
export interface Gpu {
  vendor: GpuVendor;
  name?: string;
}
export type GpuVendor = "apple" | "amd" | "intel" | "nvidia" | "qualcomm" | "other";
export interface Camera {
  make: string;
  model: string;
}

export interface Report {
  schema: 1;
  app: App;
  gpu?: Gpu;
  text: string;
  logTail?: string;
  photo?: Camera & { fileType: string };
  screenshot?: { mime: "image/png" | "image/jpeg"; data: string };
}
export interface Crash {
  schema: 1;
  app: App;
  gpu?: Gpu;
  message: string;
  location: string;
  backtrace?: string;
  logTail?: string;
  crashedOn: string;
}
export interface CameraTest {
  schema: 1;
  app: App;
  camera: Camera & { firmware?: string };
  report: string;
}
export interface RawSample {
  schema: 1;
  app: App;
  camera: Camera;
  fileType: string;
  size: number;
  sha256: string;
  licence: typeof SAMPLE_LICENCE;
  personalDataAcknowledged: true;
  note?: string;
}
export interface Stats {
  schema: 1;
  week: string;
  app: App;
  gpuVendor: GpuVendor;
  cameras: Camera[];
  tools: Partial<Record<Tool, true>>;
}

export const SAMPLE_LICENCE = "rawtonic-sample-1";
export const MAX_SAMPLE_BYTES = 100 * 1024 * 1024;
export const MAX_SCREENSHOT_BYTES = 2 * 1024 * 1024;

/** Request body caps per endpoint (bytes). */
export const BODY_CAPS = {
  report: 3 * 1024 * 1024,
  crash: 96 * 1024,
  "camera-test": 32 * 1024,
  "raw-sample": 8 * 1024,
  stats: 8 * 1024,
} as const;

/** Tools counted as used/not used in the statistics. */
export const TOOLS = [
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
export type Tool = (typeof TOOLS)[number];

const GPU_VENDORS: GpuVendor[] = ["apple", "amd", "intel", "nvidia", "qualcomm", "other"];

type Json = Record<string, unknown>;

const object = (value: unknown, field: string, required: string[], optional: string[] = []): Json => {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Invalid(field);
  const record = value as Json;
  for (const key of Object.keys(record)) {
    if (!required.includes(key) && !optional.includes(key)) throw new Invalid(`${field}.${key}`);
  }
  for (const key of required) if (!(key in record)) throw new Invalid(`${field}.${key}`);
  return record;
};

// Control characters other than tab, newline and carriage return.
// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/;

/** Free text: newlines allowed, no other control characters. */
const text = (value: unknown, field: string, max: number): string => {
  if (typeof value !== "string" || value.length > max || CONTROL.test(value)) throw new Invalid(field);
  return value;
};

/** One line: letters, digits and a little punctuation, no markup. */
const NAME = /^[\p{L}\p{N} ._()+/,&'-]+$/u;
const name = (value: unknown, field: string, max = 64): string => {
  if (typeof value !== "string") throw new Invalid(field);
  const trimmed = value.trim();
  if (trimmed.length === 0 || trimmed.length > max || !NAME.test(trimmed)) throw new Invalid(field);
  return trimmed;
};

const oneOf = <T extends string>(value: unknown, field: string, allowed: readonly T[]): T => {
  if (typeof value !== "string" || !allowed.includes(value as T)) throw new Invalid(field);
  return value as T;
};

const fileType = (value: unknown, field: string): string => {
  if (typeof value !== "string" || !/^[A-Za-z0-9]{1,10}$/.test(value)) throw new Invalid(field);
  return value;
};

const schema1 = (value: unknown) => {
  if (value !== 1) throw new Invalid("schema");
  return 1 as const;
};

const app = (value: unknown): App => {
  const v = object(value, "app", ["version", "os", "osVersion", "arch"]);
  if (typeof v.version !== "string" || !/^[0-9A-Za-z.+-]{1,32}$/.test(v.version)) throw new Invalid("app.version");
  if (typeof v.osVersion !== "string" || !/^\d{1,3}(\.\d{1,3})?$/.test(v.osVersion)) throw new Invalid("app.osVersion");
  return {
    version: v.version,
    os: oneOf(v.os, "app.os", ["macos", "windows", "linux"] as const),
    osVersion: v.osVersion,
    arch: oneOf(v.arch, "app.arch", ["aarch64", "x86_64"] as const),
  };
};

const gpu = (value: unknown): Gpu => {
  const v = object(value, "gpu", ["vendor"], ["name"]);
  return {
    vendor: oneOf(v.vendor, "gpu.vendor", GPU_VENDORS),
    ...(v.name === undefined ? {} : { name: name(v.name, "gpu.name", 100) }),
  };
};

const camera = (value: unknown, field: string, extra: string[] = []) => {
  const v = object(value, field, ["make", "model"], extra);
  return { record: v, camera: { make: name(v.make, `${field}.make`), model: name(v.model, `${field}.model`) } };
};

export const parseReport = (value: unknown): Report => {
  const v = object(value, "body", ["schema", "app", "text"], ["gpu", "logTail", "photo", "screenshot"]);
  const report: Report = { schema: schema1(v.schema), app: app(v.app), text: text(v.text, "text", 10_000) };
  if (report.text.trim().length === 0) throw new Invalid("text");
  if (v.gpu !== undefined) report.gpu = gpu(v.gpu);
  if (v.logTail !== undefined) report.logTail = text(v.logTail, "logTail", 32 * 1024);
  if (v.photo !== undefined) {
    const { record, camera: c } = camera(v.photo, "photo", ["fileType"]);
    report.photo = { ...c, fileType: fileType(record.fileType, "photo.fileType") };
  }
  if (v.screenshot !== undefined) {
    const s = object(v.screenshot, "screenshot", ["mime", "data"]);
    const mime = oneOf(s.mime, "screenshot.mime", ["image/png", "image/jpeg"] as const);
    if (
      typeof s.data !== "string" ||
      s.data.length > Math.ceil(MAX_SCREENSHOT_BYTES / 3) * 4 ||
      !/^[A-Za-z0-9+/]*={0,2}$/.test(s.data) ||
      s.data.length % 4 !== 0
    ) {
      throw new Invalid("screenshot.data");
    }
    report.screenshot = { mime, data: s.data };
  }
  return report;
};

export const parseCrash = (value: unknown): Crash => {
  const v = object(value, "body", ["schema", "app", "message", "location", "crashedOn"], ["gpu", "backtrace", "logTail"]);
  const crash: Crash = {
    schema: schema1(v.schema),
    app: app(v.app),
    message: text(v.message, "message", 2_000),
    location: text(v.location, "location", 300),
    crashedOn: (() => {
      if (typeof v.crashedOn !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(v.crashedOn)) throw new Invalid("crashedOn");
      return v.crashedOn;
    })(),
  };
  if (v.gpu !== undefined) crash.gpu = gpu(v.gpu);
  if (v.backtrace !== undefined) crash.backtrace = text(v.backtrace, "backtrace", 32 * 1024);
  if (v.logTail !== undefined) crash.logTail = text(v.logTail, "logTail", 32 * 1024);
  return crash;
};

export const parseCameraTest = (value: unknown): CameraTest => {
  const v = object(value, "body", ["schema", "app", "camera", "report"]);
  const { record, camera: c } = camera(v.camera, "camera", ["firmware"]);
  return {
    schema: schema1(v.schema),
    app: app(v.app),
    camera: record.firmware === undefined ? c : { ...c, firmware: name(record.firmware, "camera.firmware") },
    report: text(v.report, "report", 16 * 1024),
  };
};

export const parseRawSample = (value: unknown): RawSample => {
  const v = object(
    value,
    "body",
    ["schema", "app", "camera", "fileType", "size", "sha256", "licence", "personalDataAcknowledged"],
    ["note"],
  );
  if (typeof v.size !== "number" || !Number.isInteger(v.size) || v.size < 1 || v.size > MAX_SAMPLE_BYTES) {
    throw new Invalid("size");
  }
  if (typeof v.sha256 !== "string" || !/^[0-9a-f]{64}$/.test(v.sha256)) throw new Invalid("sha256");
  if (v.licence !== SAMPLE_LICENCE) throw new Invalid("licence");
  if (v.personalDataAcknowledged !== true) throw new Invalid("personalDataAcknowledged");
  const sample: RawSample = {
    schema: schema1(v.schema),
    app: app(v.app),
    camera: camera(v.camera, "camera").camera,
    fileType: fileType(v.fileType, "fileType"),
    size: v.size,
    sha256: v.sha256,
    licence: SAMPLE_LICENCE,
    personalDataAcknowledged: true,
  };
  if (v.note !== undefined) sample.note = text(v.note, "note", 1_000);
  return sample;
};

export const parseStats = (value: unknown): Stats => {
  const v = object(value, "body", ["schema", "week", "app", "gpuVendor", "cameras", "tools"]);
  if (typeof v.week !== "string" || !/^\d{4}-W\d{2}$/.test(v.week)) throw new Invalid("week");
  if (!Array.isArray(v.cameras) || v.cameras.length > 50) throw new Invalid("cameras");
  const cameras = v.cameras.map((c, i) => camera(c, `cameras.${i}`).camera);
  const tools = object(v.tools, "tools", [], [...TOOLS]);
  for (const [key, used] of Object.entries(tools)) if (used !== true) throw new Invalid(`tools.${key}`);
  return {
    schema: schema1(v.schema),
    week: v.week,
    app: app(v.app),
    gpuVendor: oneOf(v.gpuVendor, "gpuVendor", GPU_VENDORS),
    cameras,
    tools: tools as Stats["tools"],
  };
};

/** True when a string anywhere in the body still holds a home-folder path:
 * the app scrubs paths before the preview, so this is a second safety net. */
export const hasHomePath = (value: unknown): boolean => {
  if (typeof value === "string") return /\/Users\/[^/\s<]|\/home\/[^/\s<]|[A-Za-z]:\\Users\\/i.test(value);
  if (Array.isArray(value)) return value.some(hasHomePath);
  if (typeof value === "object" && value !== null) return Object.values(value).some(hasHomePath);
  return false;
};
