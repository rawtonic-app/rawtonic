/** UI vocabulary of the naming tokens (`rawtonic_naming`), the one naming
 * engine of ingest, rename and export. Never offer an unsupported token. */
export type NamingMode = "ingest" | "rename" | "export";
/** What a template names: a file, or the folders below a destination. */
export type NamingKind = "file" | "folder";
export type NamingGroup = "name" | "counter" | "date" | "camera" | "metadata" | "job";
export type NamingTokenLabel =
  | "original"
  | "originalWithExtension"
  | "sequence"
  | "job"
  | "format"
  | "recipe"
  | "date"
  | "year"
  | "month"
  | "day"
  | "hour"
  | "minute"
  | "extension"
  | "yearShort"
  | "monthName"
  | "monthShort"
  | "dayOfYear"
  | "weekday"
  | "second"
  | "subsecond"
  | "timeZone"
  | "time"
  | "fileNumber"
  | "folder"
  | "make"
  | "model"
  | "camera"
  | "serial"
  | "lens"
  | "iso"
  | "focalLength"
  | "aperture"
  | "shutter"
  | "rating"
  | "colorLabel"
  | "title"
  | "creator"
  | "copyright";
export type NamingToken = {
  /** The token as a template holds it, `{orig_base}`. */
  value: string;
  name: string;
  label: NamingTokenLabel;
  group: NamingGroup;
};
/** Mirrors `rawtonic_naming::DateSource`: where a photo's capture date
 * came from, best first. */
export type DateSource = "original" | "digitized" | "fileDate" | "modified" | "now";
/** Mirrors `rawtonic_ingest::NamePreview`. */
export interface NamePreview {
  name: string;
  dateSource: DateSource;
  sidecar: boolean;
}
/** The live example the editor shows: the backend's name for the selected
 * photo (`session_naming_preview`, `export_naming_preview`, the rename
 * plan), with where its date came from when the backend says so. */
export interface NamingPreview {
  name: string;
  folder?: string;
  dateSource?: DateSource;
  sidecar?: boolean;
}

const TOKEN_LIST: [string, NamingTokenLabel, NamingGroup][] = [
  ["orig_base", "original", "name"],
  ["orig", "originalWithExtension", "name"],
  ["orig_num", "fileNumber", "name"],
  ["folder", "folder", "name"],
  ["ext", "extension", "name"],
  ["seq", "sequence", "counter"],
  ["date", "date", "date"],
  ["time", "time", "date"],
  ["yyyy", "year", "date"],
  ["yy", "yearShort", "date"],
  ["mm", "month", "date"],
  ["month", "monthName", "date"],
  ["mon", "monthShort", "date"],
  ["dd", "day", "date"],
  ["doy", "dayOfYear", "date"],
  ["weekday", "weekday", "date"],
  ["hh", "hour", "date"],
  ["MM", "minute", "date"],
  ["ss", "second", "date"],
  ["subsec", "subsecond", "date"],
  ["tz", "timeZone", "date"],
  ["camera", "camera", "camera"],
  ["make", "make", "camera"],
  ["model", "model", "camera"],
  ["serial", "serial", "camera"],
  ["lens", "lens", "camera"],
  ["iso", "iso", "camera"],
  ["focal", "focalLength", "camera"],
  ["aperture", "aperture", "camera"],
  ["shutter", "shutter", "camera"],
  ["rating", "rating", "metadata"],
  ["color", "colorLabel", "metadata"],
  ["title", "title", "metadata"],
  ["creator", "creator", "metadata"],
  ["copyright", "copyright", "metadata"],
  ["job", "job", "job"],
  ["format", "format", "job"],
  ["recipe", "recipe", "job"],
];

/** The tokens a mode fills: `{job}` is the library's name on import and
 * the job name of an export (a rename has none); `{format}` and `{recipe}`
 * exist for exports only. */
const MODE_TOKENS: Record<NamingMode, (name: string) => boolean> = {
  ingest: (name) => name !== "format" && name !== "recipe",
  rename: (name) => !["job", "format", "recipe"].includes(name),
  export: () => true,
};

export const NAMING_GROUPS: NamingGroup[] = ["name", "counter", "date", "camera", "metadata", "job"];

export const namingTokens = (mode: NamingMode): NamingToken[] =>
  TOKEN_LIST.filter(([name]) => MODE_TOKENS[mode](name)).map(([name, label, group]) => ({
    value: name === "seq" ? "{seq:auto}" : `{${name}}`,
    name,
    label,
    group,
  }));

/** Mirrors `rawtonic_naming::DATE_FORMATS` / `TIME_FORMATS`: the formats of
 * the one date (time) token, `{date:yyyymmdd}`; the first is `{date}`. */
export const DATE_FORMATS = [
  "yyyy-mm-dd",
  "yyyymmdd",
  "yymmdd",
  "yyyy_mm_dd",
  "dd-mm-yyyy",
  "dd.mm.yyyy",
  "ddmmyyyy",
  "mm-dd-yyyy",
] as const;
export const TIME_FORMATS = ["hh-MM-ss", "hhMMss", "hh.MM.ss", "hhMM"] as const;
/** Counter digits offered: `auto` fits the batch (an import, whose end is
 * not known, counts without padding then), else a fixed width. */
export const COUNTER_DIGITS = ["auto", "1", "2", "3", "4", "5", "6"] as const;

/** Mirrors `rawtonic_naming::TOKENS`, every token a template may use. */
export const INGEST_TOKEN_NAMES = [
  "yyyy",
  "yy",
  "mm",
  "month",
  "mon",
  "dd",
  "doy",
  "weekday",
  "hh",
  "MM",
  "ss",
  "subsec",
  "tz",
  "date",
  "time",
  "orig",
  "orig_base",
  "orig_num",
  "folder",
  "ext",
  "seq",
  "seq_start",
  "job",
  "session",
  "version",
  "user_text",
  "make",
  "model",
  "camera",
  "serial",
  "lens",
  "iso",
  "focal",
  "aperture",
  "shutter",
  "rating",
  "color",
  "title",
  "creator",
  "copyright",
  "format",
  "recipe",
] as const;

/** The parts of a template: tokens (`{…}`), free text, stray braces. */
export function splitTemplate(value: string): string[] {
  return value.match(/\{[^{}]*\}|[^{}]+|[{}]/g) ?? [];
}
export const isToken = (part: string) => /^\{[^{}]*\}$/.test(part);

/** A token's name, its option (counter width or date format) and
 * transforms: `{seq:4|upper}` → `seq`, `4`, `["upper"]`. */
export function parseToken(part: string): { name: string; option?: string; transforms: string[] } {
  const [token, ...transforms] = part.slice(1, -1).split("|");
  const at = token.indexOf(":");
  return at < 0
    ? { name: token, transforms }
    : { name: token.slice(0, at), option: token.slice(at + 1), transforms };
}
export function formatToken(name: string, option: string | undefined, transforms: string[]): string {
  return `{${name}${option !== undefined ? `:${option}` : ""}${transforms.map((t) => `|${t}`).join("")}}`;
}

/** A template as the chip field edits it: free text before, between and
 * after its tokens (each possibly empty), so `text[i]` is followed by
 * `tokens[i]`. Joining them gives the template back unchanged. */
export function templateSegments(template: string): { text: string[]; tokens: string[] } {
  const text = [""];
  const tokens: string[] = [];
  for (const part of splitTemplate(template)) {
    if (isToken(part)) {
      tokens.push(part);
      text.push("");
    } else text[text.length - 1] += part;
  }
  return { text, tokens };
}
export const joinSegments = (text: string[], tokens: string[]) =>
  text.map((t, i) => t + (tokens[i] ?? "")).join("");

/** Mirrors `rawtonic_naming::translate_legacy_export`: an export (or
 * rename) template in the first export tokens, where `{MM}` was the month
 * and `{mm}` the minute, written with the naming tokens. */
const LEGACY_TOKENS: [string, string][] = [
  ["{original_filename}", "{orig_base}"],
  ["{sequence}", "{seq:auto}"],
  ["{YYYY}", "{yyyy}"],
  ["{MM}", "{mm}"],
  ["{DD}", "{dd}"],
  ["{hh}", "{hh}"],
  ["{mm}", "{MM}"],
];
export function translateLegacyExport(template: string): string {
  let output = "";
  let rest = template;
  outer: while (rest) {
    for (const [old, next] of LEGACY_TOKENS)
      if (rest.startsWith(old)) {
        output += next;
        rest = rest.slice(old.length);
        continue outer;
      }
    output += rest[0];
    rest = rest.slice(1);
  }
  return output;
}

export function namingPresets(
  mode: NamingMode,
): { name: "original" | "dateOriginal" | "numbered"; template: string }[] {
  return [
    { name: "original", template: mode === "ingest" ? "{orig}" : "{orig_base}" },
    { name: "dateOriginal", template: "{date:yyyymmdd}_{orig_base}" },
    {
      name: "numbered",
      template: mode === "ingest" ? "{job}_{seq:4}" : mode === "export" ? "{job}_{seq:auto}" : "{folder}_{seq:auto}",
    },
  ];
}

const pad2 = (n: number) => String(n).padStart(2, "0");
const EXAMPLE_DATE = new Date(2026, 9, 11, 14, 30, 0);
function formatDate(pattern: string, date: Date): string {
  const parts: Record<string, string> = {
    yyyy: String(date.getFullYear()),
    yy: pad2(date.getFullYear() % 100),
    mm: pad2(date.getMonth() + 1),
    dd: pad2(date.getDate()),
    hh: pad2(date.getHours()),
    MM: pad2(date.getMinutes()),
    ss: pad2(date.getSeconds()),
  };
  return pattern.replace(/yyyy|yy|mm|dd|hh|MM|ss/g, (m) => parts[m]);
}

/** Illustrative example, labelled as such: the backend fills real values. */
export function namingExample(
  template: string,
  mode: NamingMode,
  original: string,
  extension = "jpg",
  job = "Project",
  sequence = 1,
  total = 1,
  kind: NamingKind = "file",
): string {
  const filename = original.split(/[\\/]/).pop() || "DSC01234.RAF";
  const stem = filename.replace(/\.[^.]+$/, "");
  const ext = filename.includes(".") ? filename.split(".").pop()! : "";
  const d = EXAMPLE_DATE;
  const values: Record<string, string> = {
    orig: filename,
    orig_base: stem,
    ext,
    seq: String(sequence),
    seq_start: String(sequence),
    job: mode === "rename" ? "" : job,
    session: job,
    format: extension.toLowerCase() === "jpg" ? "JPEG" : extension.toLowerCase() === "tif" ? "TIFF" : extension.toUpperCase(),
    recipe: "Recipe",
    date: formatDate(DATE_FORMATS[0], d),
    time: formatDate(TIME_FORMATS[0], d),
    yyyy: formatDate("yyyy", d),
    mm: formatDate("mm", d),
    dd: formatDate("dd", d),
    hh: formatDate("hh", d),
    MM: formatDate("MM", d),
    ss: formatDate("ss", d),
    yy: formatDate("yy", d),
    month: "October",
    mon: "Oct",
    doy: "284",
    weekday: "Sunday",
    subsec: "00",
    tz: "+0200",
    orig_num: stem.match(/\d*$/)?.[0] ?? "",
    folder: "100CAMERA",
    make: "Camera",
    model: "Model",
    camera: "Camera Model",
    serial: "0123456789",
    lens: "50mm F1.8",
    iso: "400",
    focal: "50",
    aperture: "2.8",
    shutter: "1-250",
    rating: "0",
    color: "",
    title: "",
    creator: "",
    copyright: "",
  };
  const last = sequence + Math.max(0, total - 1);
  const rendered = template.replace(/\{([^{}]+)\}/g, (match, expression: string) => {
    const { name, option, transforms } = parseToken(`{${expression}}`);
    if (!(name in values)) return match;
    let value = values[name];
    if (name === "seq" && option === "auto") value = value.padStart(String(last).length, "0");
    else if (name === "seq" && option && /^\d+$/.test(option))
      value = value.padStart(Math.min(20, Number(option)), "0");
    else if (name === "date" && option && (DATE_FORMATS as readonly string[]).includes(option))
      value = formatDate(option, d);
    else if (name === "time" && option && (TIME_FORMATS as readonly string[]).includes(option))
      value = formatDate(option, d);
    else if (option !== undefined) return match;
    for (const transform of transforms) {
      if (transform === "upper") value = value.toUpperCase();
      else if (transform === "lower") value = value.toLowerCase();
      else if (/^pad:\d+$/.test(transform)) value = value.padStart(Math.min(20, Number(transform.slice(4))), "0");
      else return match;
    }
    return Array.from(value, (c) => (c.charCodeAt(0) < 32 || '/\\:*?"<>|'.includes(c) ? "_" : c)).join("");
  });
  if (kind === "folder")
    return rendered
      .split(/[/\\]/)
      .map((part) => part.trim().replace(/[. ]+$/, ""))
      .filter(Boolean)
      .join("/");
  // Mirrors `render_or_original`: a name its tokens leave empty is the original's.
  const name = rendered.trim() ? rendered : stem;
  if (mode === "export") return `${name}.${extension}`;
  // Mirrors `without_extension` in the rename plan.
  if (mode === "rename")
    return !ext || name.toLowerCase().endsWith(`.${ext.toLowerCase()}`) ? name : `${name}.${ext}`;
  // Mirrors `rawtonic_ingest::render_name`: a copy keeps its photograph's
  // extension when the template leaves it out.
  const kept = !ext || name.toLowerCase().endsWith(`.${ext.toLowerCase()}`);
  return kept ? name : `${name}.${ext}`;
}

const width = (s: string) => /^\d+$/.test(s) && Number(s) >= 1 && Number(s) <= 20;
const RESERVED = '{}/\\:*?"<>|';
function validText(part: string, kind: NamingKind): boolean {
  const reserved = kind === "folder" ? RESERVED.replace("/", "").replace("\\", "") : RESERVED;
  return !Array.from(part).some((c) => reserved.includes(c) || c.charCodeAt(0) < 32);
}
function validTokenPart(part: string, mode: NamingMode): boolean {
  if (!isToken(part)) return false;
  const { name, option, transforms } = parseToken(part);
  // Every token the backend fills for the mode, also those the lists do not
  // offer (`{seq_start}`, `{session}` of older import presets).
  if (!(INGEST_TOKEN_NAMES as readonly string[]).includes(name) || !MODE_TOKENS[mode](name)) return false;
  if (option !== undefined) {
    const ok =
      (name === "seq" && (option === "auto" || width(option))) ||
      (name === "date" && (DATE_FORMATS as readonly string[]).includes(option)) ||
      (name === "time" && (TIME_FORMATS as readonly string[]).includes(option));
    if (!ok) return false;
  }
  return transforms.every((tr) => tr === "upper" || tr === "lower" || (tr.startsWith("pad:") && width(tr.slice(4))));
}

/** Catch typos before a copy/export starts; final filesystem validation stays in Rust. */
export function validNamingTemplate(template: string, mode: NamingMode, kind: NamingKind = "file"): boolean {
  if (template.length > 4096) return false;
  if (kind === "file" && !template.trim()) return false;
  for (const part of splitTemplate(template)) {
    if (part.startsWith("{")) {
      if (!validTokenPart(part, mode)) return false;
    } else if (!validText(part, kind)) return false;
  }
  if (kind === "folder") {
    // Mirrors `rawtonic_naming::render_folder`: no `..`, no drive or root.
    const trimmed = template.trim();
    if (/^[a-zA-Z]:/.test(trimmed)) return false;
    if (trimmed.split(/[/\\]/).some((p) => /^\s*\.+\s*$/.test(p))) return false;
  }
  return true;
}

export const MAX_SEQUENCE_START = 4_294_967_295;
export const validSequenceStart = (value: number) =>
  Number.isInteger(value) && value >= 0 && value <= MAX_SEQUENCE_START;

/** The counter width every `{seq}` of `template` uses: `auto`, a number,
 * `""` (unpadded), or null when it has none or they differ. */
export function counterDigits(template: string): string | null {
  const widths = new Set(
    splitTemplate(template)
      .filter((p) => isToken(p) && parseToken(p).name === "seq")
      .map((p) => parseToken(p).option ?? ""),
  );
  return widths.size === 1 ? [...widths][0] : null;
}
/** `template` with every `{seq}` at width `digits` (`""`: unpadded). */
export function withCounterDigits(template: string, digits: string): string {
  return splitTemplate(template)
    .map((p) => {
      if (!isToken(p)) return p;
      const { name, transforms } = parseToken(p);
      return name === "seq" ? formatToken(name, digits || undefined, transforms) : p;
    })
    .join("");
}

/** Insert at a boundary in the original list, optionally moving an existing block. */
export function dropNamingBlock(parts: string[], target: number, text: string, source?: number): string {
  if (!Number.isInteger(target) || target < 0 || target > parts.length) return parts.join("");
  const next = [...parts];
  if (source !== undefined) {
    if (!Number.isInteger(source) || source < 0 || source >= parts.length) return parts.join("");
    text = next.splice(source, 1)[0];
    if (source < target) target--;
  }
  next.splice(target, 0, text);
  return next.join("");
}

/** A saved naming preset, shared by import, rename and export. */
export type SavedNaming = { name: string; template: string; sequenceStart?: number };
export const NAMING_PRESETS_KEY = "rawtonic.naming.presets";
const MAX_PRESETS = 50;
const validSaved = (p: unknown): p is SavedNaming =>
  !!p &&
  typeof (p as SavedNaming).name === "string" &&
  typeof (p as SavedNaming).template === "string";
function readList(key: string): SavedNaming[] {
  try {
    const items: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
    return Array.isArray(items) ? items.filter(validSaved) : [];
  } catch {
    return [];
  }
}
/** The user's naming presets. Presets saved before the editor was shared
 * (per mode, export ones in the first export tokens) are taken over once:
 * export ones translated, a name taken twice kept as the import one. */
export function readNamingPresets(): SavedNaming[] {
  try {
    if (localStorage.getItem(NAMING_PRESETS_KEY) === null) {
      const ingest = readList("rawtonic.naming.ingest");
      const exported = readList("rawtonic.naming.export").map((p) => ({
        ...p,
        template: translateLegacyExport(p.template),
      }));
      const merged = [...ingest];
      for (const p of exported)
        if (!merged.some((m) => m.name === p.name)) merged.push(p);
      if (merged.length) localStorage.setItem(NAMING_PRESETS_KEY, JSON.stringify(merged.slice(0, MAX_PRESETS)));
    }
  } catch {
    // Storage unavailable: the built-in presets still work.
  }
  return readList(NAMING_PRESETS_KEY).slice(0, MAX_PRESETS);
}
export function writeNamingPresets(presets: SavedNaming[]): void {
  localStorage.setItem(NAMING_PRESETS_KEY, JSON.stringify(presets.slice(0, MAX_PRESETS)));
}

/** `template` with `token` inserted into its free text `slot` (before
 * token `slot`) at character `offset`. */
export function insertToken(template: string, slot: number, offset: number, token: string): string {
  const { text, tokens } = templateSegments(template);
  const at = Math.max(0, Math.min(slot, text.length - 1));
  const before = text[at].slice(0, offset);
  const after = text[at].slice(offset);
  text.splice(at, 1, before, after);
  tokens.splice(at, 0, token);
  return joinSegments(text, tokens);
}
/** `template` without token `index`; the free text around it joins. */
export function removeToken(template: string, index: number): string {
  const { text, tokens } = templateSegments(template);
  if (index < 0 || index >= tokens.length) return template;
  tokens.splice(index, 1);
  text.splice(index, 2, text[index] + text[index + 1]);
  return joinSegments(text, tokens);
}
/** `template` with token `index` moved to the end of free text `slot`
 * (the slot numbers of the template before the move). */
export function moveToken(template: string, index: number, slot: number): string {
  const { text, tokens } = templateSegments(template);
  if (index < 0 || index >= tokens.length || slot < 0 || slot >= text.length) return template;
  // Dropped at the end of the text before it, it keeps its place.
  if (slot === index) return template;
  const token = tokens[index];
  const removed = removeToken(template, index);
  // The slots after the removed token shift down by one.
  const target = slot > index ? slot - 1 : slot;
  const { text: t2 } = templateSegments(removed);
  return insertToken(removed, target, t2[target].length, token);
}
/** `template` with tokens `index` and `index + direction` swapped (the
 * keyboard's move). */
export function swapTokens(template: string, index: number, direction: -1 | 1): string {
  const { text, tokens } = templateSegments(template);
  const other = index + direction;
  if (index < 0 || other < 0 || index >= tokens.length || other >= tokens.length) return template;
  [tokens[index], tokens[other]] = [tokens[other], tokens[index]];
  return joinSegments(text, tokens);
}
