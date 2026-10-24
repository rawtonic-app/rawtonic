// SPDX-License-Identifier: AGPL-3.0-or-later
// The issue text for each kind of submission. User text and logs go into
// fenced blocks only, so they cannot mention people or add markup; titles
// hold only validated names and versions.

import type { App, CameraTest, Crash, Gpu, Report } from "./schema";
import type { NewIssue } from "./github";
import { fence } from "./util";

/** GitHub refuses issue bodies above 65,536 characters; a margin below. */
export const BODY_LIMIT = 65_000;

const system = (app: App, gpu?: Gpu) =>
  [
    `- Rawtonic ${app.version}, ${app.os} ${app.osVersion} (${app.arch})`,
    gpu ? `- GPU: ${gpu.vendor}${gpu.name ? ` — ${gpu.name}` : ""}` : null,
  ].filter((line) => line !== null);

const footer = (ref: string) =>
  `\n---\nReference ${ref}. Filed by the Rawtonic relay (ADR 0017); deleted after 90 days or when the sender deletes it by its id.`;

/** A long part of the body and which end of it matters (a log's last
 * lines, a backtrace's first frames). */
interface Part {
  text: string;
  keep: "start" | "end";
}

/** Renders the body, shortening its longest part by a quarter until it
 * fits (fences grow with the backtick runs inside, so the rendered length
 * is what counts). */
const fit = <K extends string>(
  parts: Record<K, Part | undefined>,
  render: (texts: Record<K, string | undefined>) => string,
): string => {
  const current = { ...parts };
  for (;;) {
    const entries = Object.entries(current) as [K, Part | undefined][];
    const texts = Object.fromEntries(entries.map(([k, p]) => [k, p?.text])) as Record<K, string | undefined>;
    const body = render(texts);
    if (body.length <= BODY_LIMIT) return body;
    const [key, longest] = entries
      .filter((entry): entry is [K, Part] => entry[1] !== undefined)
      .sort((a, b) => b[1].text.length - a[1].text.length)[0];
    const keep = Math.floor(longest.text.length * 0.75);
    const note = `[${longest.text.length - keep} characters left out]`;
    current[key] = {
      keep: longest.keep,
      text:
        longest.keep === "end"
          ? `${note}\n${longest.text.slice(longest.text.length - keep)}`
          : `${longest.text.slice(0, keep)}\n${note}`,
    };
  }
};

/** `keys`: where the report's announced files go in the bucket; each
 * arrives separately, so one may be missing if its upload did not finish. */
export const reportIssue = (r: Report, ref: string, keys: { screenshot?: string; photo?: string } = {}): NewIssue => ({
  title: `Problem report ${ref} · Rawtonic ${r.app.version} · ${r.app.os}`,
  labels: ["report"],
  body: fit(
    { text: { text: r.text, keep: "start" }, log: r.logTail ? { text: r.logTail, keep: "end" } : undefined },
    ({ text, log }) =>
      [
        "## System",
        ...system(r.app, r.gpu),
        r.photo ? `- Photo: ${r.photo.make} ${r.photo.model}, ${r.photo.fileType}` : null,
        keys.photo && r.photo?.file ? `- Photo file (${r.photo.file.size} bytes, if uploaded): R2 \`${keys.photo}\`` : null,
        keys.screenshot ? `- Screenshot (if uploaded): R2 \`${keys.screenshot}\`` : null,
        "",
        "## What happened",
        fence(text ?? ""),
        log ? `\n## Log (last lines, scrubbed)\n${fence(log)}` : null,
        footer(ref),
      ]
        .filter((line) => line !== null)
        .join("\n"),
  ),
});

export const crashIssue = (c: Crash, ref: string): NewIssue => ({
  title: `Crash ${ref} · Rawtonic ${c.app.version} · ${c.app.os}`,
  labels: ["crash"],
  body: fit(
    {
      panic: { text: `${c.message}\n${c.location}`, keep: "start" },
      backtrace: c.backtrace ? { text: c.backtrace, keep: "start" } : undefined,
      log: c.logTail ? { text: c.logTail, keep: "end" } : undefined,
    },
    ({ panic, backtrace, log }) =>
      [
        "## System",
        ...system(c.app, c.gpu),
        `- Crashed on ${c.crashedOn}`,
        "",
        "## Panic",
        fence(panic ?? ""),
        backtrace ? `\n## Backtrace\n${fence(backtrace)}` : null,
        log ? `\n## Log (last lines, scrubbed)\n${fence(log)}` : null,
        footer(ref),
      ]
        .filter((line) => line !== null)
        .join("\n"),
  ),
});

export const cameraTestIssue = (t: CameraTest, ref: string): NewIssue => ({
  title: `Camera test ${ref} · ${t.camera.make} ${t.camera.model}`,
  labels: ["camera-test"],
  body: fit({ report: { text: t.report, keep: "start" } }, ({ report }) =>
    [
      "## System",
      ...system(t.app),
      `- Camera: ${t.camera.make} ${t.camera.model}${t.camera.firmware ? `, firmware ${t.camera.firmware}` : ""}`,
      "",
      "## Result",
      fence(report ?? "", "markdown"),
      footer(ref),
    ].join("\n"),
  ),
});
