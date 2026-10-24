// SPDX-License-Identifier: AGPL-3.0-or-later
// Problem and crash reports (ADR 0017, channels 1 and 2): their payloads,
// and the text the no-relay fallbacks use ("Copy report", "Open on GitHub").
// The fallbacks take the scrubbed body the app prepared, never raw input.
import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../ui/AppProperties";

export interface AppInfo {
  version: string;
  os: string;
  osVersion: string;
  arch: string;
}

export interface GpuInfo {
  vendor: string;
  name?: string;
}

/** What the app adds to a problem report besides the user's text. */
export interface ReportContext {
  app: AppInfo;
  gpu?: GpuInfo | null;
  logTail?: string | null;
  screenshotAvailable: boolean;
}

export interface Screenshot {
  mime: "image/jpeg" | "image/png";
  data: string;
  width: number;
  height: number;
}

export interface PhotoInfo {
  make: string;
  model: string;
  fileType: string;
}

/** A crash the previous run left (src-tauri/src/contribute.rs). */
export interface CrashRecord {
  id: string;
  crashedOn: string;
  created: number;
  app: AppInfo;
  gpu?: GpuInfo | null;
  message: string;
  location: string;
  backtrace?: string | null;
  logTail?: string | null;
  asked: boolean;
}

/** The relay's caps (services/relay/src/schema.ts). */
export const TEXT_CAP = 10_000;
export const DIAGNOSTICS_CAP = 24 * 1024;

export const problemReportContext = () => invoke<ReportContext>(Invokes.ProblemReportContext);
export const problemReportScreenshot = () => invoke<Screenshot | null>(Invokes.ProblemReportScreenshot);
export const pendingCrashReports = () => invoke<CrashRecord[]>(Invokes.CrashReportsPending);
export const markCrashReportAsked = (id: string) => invoke<void>(Invokes.CrashReportMarkAsked, { id });
export const removeCrashReport = (id: string) => invoke<void>(Invokes.CrashReportRemove, { id });
export const clearCrashReports = () => invoke<void>(Invokes.CrashReportsClear);

/** One line of letters, digits and the punctuation the relay's names take. */
export const plainName = (text: string | undefined, max = 64): string | null => {
  const kept = (text ?? "")
    .replace(/\s+/gu, " ")
    .replace(/[^\p{L}\p{N} ._()+/,&'-]/gu, "")
    .replace(/ {2,}/g, " ")
    .slice(0, max)
    .trim();
  return kept || null;
};

/** Camera make and model and the file type of a photo, as the relay takes them. */
export const photoInfo = (
  photo: { path: string; exif: Record<string, string> | null } | null | undefined,
): PhotoInfo | null => {
  if (!photo) return null;
  const make = plainName(photo.exif?.Make);
  const model = plainName(photo.exif?.Model);
  // A virtual copy's path carries "?vc=…" after the file name.
  const name = photo.path.split("?")[0].split(/[\\/]/).pop() ?? "";
  const dot = name.lastIndexOf(".");
  const fileType = dot > 0 ? name.slice(dot + 1) : "";
  if (!make || !model || !/^[A-Za-z0-9]{1,10}$/.test(fileType)) return null;
  return { make, model, fileType: fileType.toUpperCase() };
};

export interface ReportChoices {
  text: string;
  context: ReportContext;
  includeSystem: boolean;
  /** The log tail as the user left it, or null to leave it out. */
  logTail: string | null;
  photo: PhotoInfo | null;
  screenshot: Screenshot | null;
}

/** The `POST /report` payload (schema 1) for the user's choices. */
export const reportPayload = (c: ReportChoices): Record<string, unknown> => {
  const payload: Record<string, unknown> = { schema: 1, app: c.context.app, text: c.text.trim() };
  if (c.includeSystem && c.context.gpu) payload.gpu = c.context.gpu;
  if (c.logTail?.trim()) payload.logTail = c.logTail;
  if (c.photo) payload.photo = c.photo;
  if (c.screenshot) payload.screenshot = { mime: c.screenshot.mime, data: c.screenshot.data };
  return payload;
};

export interface CrashChoices {
  includeBacktrace: boolean;
  /** The log tail as the user left it, or null to leave it out. */
  logTail: string | null;
}

/** The `POST /crash` payload (schema 1) of a record and the user's choices. */
export const crashPayload = (record: CrashRecord, c: CrashChoices): Record<string, unknown> => {
  const payload: Record<string, unknown> = {
    schema: 1,
    app: record.app,
    message: record.message,
    location: record.location,
    crashedOn: record.crashedOn,
  };
  if (record.gpu) payload.gpu = record.gpu;
  if (c.includeBacktrace && record.backtrace) payload.backtrace = record.backtrace;
  if (c.logTail?.trim()) payload.logTail = c.logTail;
  return payload;
};

/** A Markdown code block that no backtick run inside `text` can close. */
const fenced = (text: string) => {
  const longest = Math.max(2, ...(text.match(/`+/g) ?? []).map((run) => run.length));
  const fence = "`".repeat(longest + 1);
  return `${fence}\n${text}\n${fence}`;
};

interface Parsed {
  app?: AppInfo;
  gpu?: GpuInfo;
  text?: string;
  logTail?: string;
  photo?: PhotoInfo;
  screenshot?: unknown;
  message?: string;
  location?: string;
  backtrace?: string;
  crashedOn?: string;
}

const systemLine = (p: Parsed) => {
  const parts = [
    p.app && `Rawtonic ${p.app.version}`,
    p.app && `${p.app.os} ${p.app.osVersion} (${p.app.arch})`,
    p.gpu && `GPU: ${[p.gpu.vendor, p.gpu.name].filter(Boolean).join(", ")}`,
  ].filter(Boolean);
  return parts.join(" · ");
};

/** How much of the long parts a text keeps (for the GitHub URL's limit). */
interface Fit {
  logLines: number;
  backtraceLines: number;
  textChars: number;
}

const FULL: Fit = { logLines: Infinity, backtraceLines: Infinity, textChars: Infinity };
const SHORTENED_NOTE = "(Shortened to fit a link. Use “Copy report” in Rawtonic for the full text.)";

function render(p: Parsed, fit: Fit): string {
  const out: string[] = [];
  const shortened = { value: false };
  const keep = <T,>(items: T[], n: number, fromEnd: boolean): T[] => {
    if (items.length <= n) return items;
    shortened.value = true;
    return fromEnd ? items.slice(items.length - n) : items.slice(0, n);
  };
  if (p.message !== undefined) {
    out.push(`Rawtonic quit unexpectedly on ${p.crashedOn ?? "?"} at \`${p.location ?? "?"}\`.`);
    out.push(fenced(p.message));
  }
  if (p.text !== undefined) {
    let text = p.text;
    if (text.length > fit.textChars) {
      text = `${text.slice(0, Math.max(0, fit.textChars))}…`;
      shortened.value = true;
    }
    out.push(text);
  }
  const system = systemLine(p);
  if (system) out.push(`**System:** ${system}`);
  if (p.photo) out.push(`**Photo:** ${p.photo.make} ${p.photo.model} (${p.photo.fileType})`);
  if (p.screenshot) out.push("**Screenshot:** not included here (only the relay takes images).");
  if (p.backtrace) {
    const lines = keep(p.backtrace.split("\n"), fit.backtraceLines, false);
    if (lines.length) out.push(`**Backtrace**\n\n${fenced(lines.join("\n"))}`);
  }
  if (p.logTail) {
    const lines = keep(p.logTail.split("\n"), fit.logLines, true);
    if (lines.length) out.push(`**Log (last lines)**\n\n${fenced(lines.join("\n"))}`);
  }
  if (shortened.value) out.push(SHORTENED_NOTE);
  return out.join("\n\n");
}

const parse = (body: string): Parsed => {
  try {
    return JSON.parse(body) as Parsed;
  } catch {
    return { text: body };
  }
};

/** The prepared (scrubbed) body as plain Markdown, for "Copy report". */
export const reportMarkdown = (body: string): string => render(parse(body), FULL);

export const ISSUE_BASE = "https://github.com/rawtonic-app/rawtonic/issues/new";
/** Links GitHub and browsers take reliably. */
export const MAX_URL = 8_000;

const issueTitle = (p: Parsed) => {
  const first = (p.message ?? p.text ?? "").split("\n")[0].trim();
  const short = first.length > 70 ? `${first.slice(0, 70)}…` : first;
  return p.message !== undefined ? `Crash: ${short}` : `Problem: ${short}`;
};

const issueUrl = (title: string, body: string) =>
  `${ISSUE_BASE}?${new URLSearchParams({ title, body }).toString().replace(/\+/g, "%20")}`;

/** A prefilled GitHub issue for the prepared body, text only, within
 * [`MAX_URL`]: the log's oldest lines go first, then the backtrace's
 * deepest frames, then the end of the text. */
export const githubIssueUrl = (body: string): string => {
  const p = parse(body);
  const title = issueTitle(p);
  const fit: Fit = {
    logLines: p.logTail ? p.logTail.split("\n").length : 0,
    backtraceLines: p.backtrace ? p.backtrace.split("\n").length : 0,
    textChars: (p.text ?? "").length,
  };
  for (;;) {
    const url = issueUrl(title, render(p, fit));
    if (url.length <= MAX_URL) return url;
    if (fit.logLines > 0) fit.logLines = Math.floor(fit.logLines * 0.8);
    else if (fit.backtraceLines > 0) fit.backtraceLines = Math.floor(fit.backtraceLines * 0.8);
    else if (fit.textChars > 0) fit.textChars = Math.floor(fit.textChars * 0.8);
    else return issueUrl(title, SHORTENED_NOTE);
  }
};
