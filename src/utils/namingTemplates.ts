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
    | "extension";
};
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
        { value: "{job}", label: "job" },
        { value: "{date}", label: "date" },
        { value: "{yyyy}", label: "year" },
        { value: "{mm}", label: "month" },
        { value: "{dd}", label: "day" },
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

/** Catch typos before a copy/export starts; final filesystem validation stays in Rust. */
export function validNamingTemplate(
  template: string,
  mode: NamingMode,
): boolean {
  if (!template.trim() || template.length > 4096) return false;
  const ingestNames = new Set([
    "yyyy",
    "mm",
    "dd",
    "hh",
    "MM",
    "ss",
    "date",
    "time",
    "orig",
    "orig_base",
    "seq",
    "seq_start",
    "job",
    "session",
    "camera",
    "model",
    "serial",
    "lens",
    "iso",
    "rating",
    "color",
    "version",
    "ext",
    "user_text",
  ]);
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
