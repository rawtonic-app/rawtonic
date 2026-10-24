// SPDX-License-Identifier: AGPL-3.0-or-later
// Problem and crash reports (ADR 0017, channels 1 and 2): their payloads,
// and the text the no-relay fallbacks use ("Copy report", "Open on GitHub"),
// also for the camera test's result (channel 3, `cameraTestPayload`).
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
/** Off (Never, or the preference) deletes every record and stops new ones,
 * also for this run. */
export const setCrashReportsEnabled = (enabled: boolean) =>
  invoke<void>(Invokes.CrashReportsSetEnabled, { enabled });

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
  /** The message as the user left it (it can be corrected before sending). */
  message?: string;
  includeBacktrace: boolean;
  /** The log tail as the user left it, or null to leave it out. */
  logTail: string | null;
}

/** The `POST /crash` payload (schema 1) of a record and the user's choices. */
export const crashPayload = (record: CrashRecord, c: CrashChoices): Record<string, unknown> => {
  const payload: Record<string, unknown> = {
    schema: 1,
    app: record.app,
    message: c.message ?? record.message,
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
  /** A camera test's Markdown report and its camera. */
  report?: string;
  camera?: { make?: string; model?: string };
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
  messageChars: number;
  textChars: number;
}

const FULL: Fit = { logLines: Infinity, backtraceLines: Infinity, messageChars: Infinity, textChars: Infinity };
const SHORTENED_NOTE = "(Shortened to fit a link. Use “Copy report” in Rawtonic for the full text.)";

function render(p: Parsed, fit: Fit): string {
  const out: string[] = [];
  const shortened = { value: false };
  const keep = <T,>(items: T[], n: number, fromEnd: boolean): T[] => {
    if (items.length <= n) return items;
    shortened.value = true;
    return fromEnd ? items.slice(items.length - n) : items.slice(0, n);
  };
  // A camera test's report is complete in itself (the issue template's
  // field takes it as it is).
  if (p.report !== undefined) {
    const kept = p.report.length > fit.textChars ? `${p.report.slice(0, Math.max(0, fit.textChars))}…` : p.report;
    return kept === p.report ? kept : `${kept}\n\n${SHORTENED_NOTE}`;
  }
  if (p.message !== undefined) {
    out.push(`Rawtonic quit unexpectedly on ${p.crashedOn ?? "?"} at \`${p.location ?? "?"}\`.`);
    let message = p.message;
    if (message.length > fit.messageChars) {
      message = `${message.slice(0, Math.max(0, fit.messageChars))}…`;
      shortened.value = true;
    }
    out.push(fenced(message));
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
/** The camera report issue form (.github/ISSUE_TEMPLATE/camera-report.yml);
 * its `report` field takes the camera test's report. */
export const CAMERA_REPORT_TEMPLATE = "camera-report.yml";
/** Links GitHub and browsers take reliably. */
export const MAX_URL = 8_000;

const issueTitle = (p: Parsed) => {
  if (p.report !== undefined) return `Camera report: ${p.camera?.model ?? ""}`.trim();
  const first = (p.message ?? p.text ?? "").split("\n")[0].trim();
  const short = first.length > 70 ? `${first.slice(0, 70)}…` : first;
  return p.message !== undefined ? `Crash: ${short}` : `Problem: ${short}`;
};

const issueUrl = (p: Parsed, title: string, body: string) => {
  const query: Record<string, string> =
    p.report !== undefined ? { template: CAMERA_REPORT_TEMPLATE, title, report: body } : { title, body };
  return `${ISSUE_BASE}?${new URLSearchParams(query).toString().replace(/\+/g, "%20")}`;
};

export interface GithubIssue {
  title: string;
  body: string;
  /** The issue form's link with `title` and `body` in its query. */
  url: string;
}

/** A prefilled GitHub issue for the prepared body, text only, within
 * [`MAX_URL`]: the log's oldest lines go first, then the backtrace's
 * deepest frames, then the end of the crash message or the text. */
export const githubIssue = (body: string): GithubIssue => {
  const p = parse(body);
  const title = issueTitle(p);
  const fit: Fit = {
    logLines: p.logTail ? p.logTail.split("\n").length : 0,
    backtraceLines: p.backtrace ? p.backtrace.split("\n").length : 0,
    messageChars: (p.message ?? "").length,
    textChars: (p.report ?? p.text ?? "").length,
  };
  for (;;) {
    const text = render(p, fit);
    const url = issueUrl(p, title, text);
    if (url.length <= MAX_URL) return { title, body: text, url };
    if (fit.logLines > 0) fit.logLines = Math.floor(fit.logLines * 0.8);
    else if (fit.backtraceLines > 0) fit.backtraceLines = Math.floor(fit.backtraceLines * 0.8);
    else if (fit.messageChars > 0) fit.messageChars = Math.floor(fit.messageChars * 0.8);
    else if (fit.textChars > 0) fit.textChars = Math.floor(fit.textChars * 0.8);
    else return { title, body: SHORTENED_NOTE, url: issueUrl(p, title, SHORTENED_NOTE) };
  }
};
