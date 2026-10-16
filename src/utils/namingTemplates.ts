/** UI vocabulary for the two existing naming engines. Never offer an unsupported token. */
export type NamingMode = "ingest" | "export";
export type NamingToken = {
  value: string;
  label:
    | "original"
    | "originalWithExtension"
    | "sequence"
    | "counter4"
    | "job"
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
export const namingTokens = (mode: NamingMode): NamingToken[] =>
  mode === "export"
    ? [
        { value: "{original_filename}", label: "original" },
        { value: "{sequence}", label: "sequence" },
        { value: "{YYYY}", label: "year" },
        { value: "{MM}", label: "month" },
        { value: "{DD}", label: "day" },
        { value: "{hh}", label: "hour" },
        { value: "{mm}", label: "minute" },
      ]
    : [
        { value: "{orig}", label: "originalWithExtension" },
        { value: "{orig_base}", label: "original" },
        { value: "{seq:4}", label: "counter4" },
        { value: "{seq}", label: "sequence" },
        { value: "{orig_num}", label: "fileNumber" },
        { value: "{folder}", label: "folder" },
        { value: "{job}", label: "job" },
        { value: "{date}", label: "date" },
        { value: "{time}", label: "time" },
        { value: "{yyyy}", label: "year" },
        { value: "{yy}", label: "yearShort" },
        { value: "{mm}", label: "month" },
        { value: "{month}", label: "monthName" },
        { value: "{mon}", label: "monthShort" },
        { value: "{dd}", label: "day" },
        { value: "{doy}", label: "dayOfYear" },
        { value: "{weekday}", label: "weekday" },
        { value: "{hh}", label: "hour" },
        { value: "{MM}", label: "minute" },
        { value: "{ss}", label: "second" },
        { value: "{subsec}", label: "subsecond" },
        { value: "{tz}", label: "timeZone" },
        { value: "{camera}", label: "camera" },
        { value: "{make}", label: "make" },
        { value: "{model}", label: "model" },
        { value: "{serial}", label: "serial" },
        { value: "{lens}", label: "lens" },
        { value: "{iso}", label: "iso" },
        { value: "{focal}", label: "focalLength" },
        { value: "{aperture}", label: "aperture" },
        { value: "{shutter}", label: "shutter" },
        { value: "{rating}", label: "rating" },
        { value: "{color}", label: "colorLabel" },
        { value: "{title}", label: "title" },
        { value: "{creator}", label: "creator" },
        { value: "{copyright}", label: "copyright" },
        { value: "{ext}", label: "extension" },
      ];
export function splitTemplate(value: string): string[] {
  return value.match(/\{[^{}]*\}|[^{}]+|[{}]/g) ?? [];
}
export function namingPresets(
  mode: NamingMode,
): { name: "original" | "dateOriginal" | "numbered"; template: string }[] {
  return mode === "export"
    ? [
        { name: "original", template: "{original_filename}" },
        {
          name: "dateOriginal",
          template: "{YYYY}{MM}{DD}_{original_filename}",
        },
        { name: "numbered", template: "{original_filename}_{sequence}" },
      ]
    : [
        { name: "original", template: "{orig}" },
        { name: "dateOriginal", template: "{yyyy}{mm}{dd}_{orig}" },
        { name: "numbered", template: "{job}_{seq:4}.{ext}" },
      ];
}
/** Illustrative preview, explicitly labelled as such: source dates are resolved by the backend. */
export function namingExample(
  template: string,
  mode: NamingMode,
  original: string,
  extension = "jpg",
  job = "Project",
  sequence = 1,
  total = 1,
): string {
  const filename = original.split(/[\\/]/).pop() || "DSC01234.RAF";
  const stem = filename.replace(/\.[^.]+$/, "");
  const ext = filename.includes(".") ? filename.split(".").pop()! : "";
  const values: Record<string, string> =
    mode === "export"
      ? {
          original_filename: stem,
          sequence: String(sequence).padStart(
            String(sequence + Math.max(0, total - 1)).length,
            "0",
          ),
          YYYY: "2026",
          MM: "10",
          DD: "11",
          hh: "14",
          mm: "30",
        }
      : {
          orig: filename,
          orig_base: stem,
          ext,
          seq: String(sequence),
          seq_start: String(sequence),
          job,
          session: job,
          date: "2026-10-11",
          time: "14-30-00",
          yyyy: "2026",
          mm: "10",
          dd: "11",
          hh: "14",
          MM: "30",
          ss: "00",
          yy: "26",
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
  const rendered = template.replace(
    /\{([^{}]+)\}/g,
    (match, expression: string) => {
      const [token, ...transforms] = expression.split("|");
      const [name, pad] = token.split(":");
      if (!(name in values)) return match;
      let value = values[name];
      if (mode === "ingest" && name === "seq" && pad && /^\d+$/.test(pad))
        value = value.padStart(Math.min(20, Number(pad)), "0");
      for (const transform of transforms) {
        if (transform === "upper") value = value.toUpperCase();
        else if (transform === "lower") value = value.toLowerCase();
        else if (/^pad:\d+$/.test(transform))
          value = value.padStart(Math.min(20, Number(transform.slice(4))), "0");
        else return match;
      }
      return Array.from(value, (c) =>
        c.charCodeAt(0) < 32 || '/\\:*?"<>|'.includes(c) ? "_" : c,
      ).join("");
    },
  );
  return mode === "export" ? `${rendered}.${extension}` : rendered;
}

/** Mirrors `rawtonic_naming::TOKENS`, every token an ingest template may use. */
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
] as const;

/** Catch typos before a copy/export starts; final filesystem validation stays in Rust. */
export function validNamingTemplate(
  template: string,
  mode: NamingMode,
): boolean {
  if (!template.trim() || template.length > 4096) return false;
  const ingestNames = new Set<string>(INGEST_TOKEN_NAMES);
  const allowed = new Set(namingTokens(mode).map((t) => t.value));
  for (const part of splitTemplate(template)) {
    if (!part.startsWith("{")) {
      if (
        Array.from(part).some((c) => '{}/\\:*?"<>|'.includes(c)) ||
        Array.from(part).some((c) => c.charCodeAt(0) < 32)
      )
        return false;
      continue;
    }
    if (!part.endsWith("}")) return false;
    if (mode === "export") {
      if (!allowed.has(part)) return false;
      continue;
    }
    const [token, ...transforms] = part.slice(1, -1).split("|");
    const [name, pad, extra] = token.split(":");
    const width = (s: string) =>
      /^\d+$/.test(s) && Number(s) >= 1 && Number(s) <= 20;
    if (
      !ingestNames.has(name) ||
      extra !== undefined ||
      (pad !== undefined && (name !== "seq" || !width(pad)))
    )
      return false;
    if (
      transforms.some(
        (tr) =>
          tr !== "upper" &&
          tr !== "lower" &&
          !(tr.startsWith("pad:") && width(tr.slice(4))),
      )
    )
      return false;
  }
  return true;
}

export const MAX_SEQUENCE_START = 4_294_967_295;
export const validSequenceStart = (value: number) =>
  Number.isInteger(value) && value >= 0 && value <= MAX_SEQUENCE_START;

/** Insert at a boundary in the original list, optionally moving an existing block. */
export function dropNamingBlock(
  parts: string[],
  target: number,
  text: string,
  source?: number,
): string {
  if (!Number.isInteger(target) || target < 0 || target > parts.length)
    return parts.join("");
  const next = [...parts];
  if (source !== undefined) {
    if (!Number.isInteger(source) || source < 0 || source >= parts.length)
      return parts.join("");
    text = next.splice(source, 1)[0];
    if (source < target) target--;
  }
  next.splice(target, 0, text);
  return next.join("");
}
