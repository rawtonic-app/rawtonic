// SPDX-License-Identifier: AGPL-3.0-or-later
// The issue text for each kind of submission. User text and logs go into
// fenced blocks only, so they cannot mention people or add markup; titles
// hold only validated names and versions.

import type { App, CameraTest, Crash, Gpu, Report } from "./schema";
import type { NewIssue } from "./github";
import { fence } from "./util";

const system = (app: App, gpu?: Gpu) =>
  [
    `- Rawtonic ${app.version}, ${app.os} ${app.osVersion} (${app.arch})`,
    gpu ? `- GPU: ${gpu.vendor}${gpu.name ? ` — ${gpu.name}` : ""}` : null,
  ].filter((line) => line !== null);

/** The marker that finds an issue again whose number was never recorded:
 * the full hash of the submission's id (the id itself is never stored). */
export const marker = (idHash: string) => `rawtonic-relay:${idHash}`;

const footer = (s: Ids) =>
  `\n---\nReference ${s.ref}. Filed by the Rawtonic relay (ADR 0017); deleted after 90 days or when the sender deletes it by its id.\n<!-- ${marker(s.idHash)} -->`;

export interface Ids {
  ref: string;
  idHash: string;
}

export const reportIssue = (r: Report, ids: Ids, screenshotKey?: string): NewIssue => ({
  title: `Problem report ${ids.ref} · Rawtonic ${r.app.version} · ${r.app.os}`,
  labels: ["report"],
  body: [
    "## System",
    ...system(r.app, r.gpu),
    r.photo ? `- Photo: ${r.photo.make} ${r.photo.model}, ${r.photo.fileType}` : null,
    screenshotKey ? `- Screenshot: R2 \`${screenshotKey}\`` : null,
    "",
    "## What happened",
    fence(r.text),
    r.logTail ? `\n## Log (last lines, scrubbed)\n${fence(r.logTail)}` : null,
    footer(ids),
  ]
    .filter((line) => line !== null)
    .join("\n"),
});

export const crashIssue = (c: Crash, ids: Ids): NewIssue => ({
  title: `Crash ${ids.ref} · Rawtonic ${c.app.version} · ${c.app.os}`,
  labels: ["crash"],
  body: [
    "## System",
    ...system(c.app, c.gpu),
    `- Crashed on ${c.crashedOn}`,
    "",
    "## Panic",
    fence(`${c.message}\n${c.location}`),
    c.backtrace ? `\n## Backtrace\n${fence(c.backtrace)}` : null,
    c.logTail ? `\n## Log (last lines, scrubbed)\n${fence(c.logTail)}` : null,
    footer(ids),
  ]
    .filter((line) => line !== null)
    .join("\n"),
});

export const cameraTestIssue = (t: CameraTest, ids: Ids): NewIssue => ({
  title: `Camera test ${ids.ref} · ${t.camera.make} ${t.camera.model}`,
  labels: ["camera-test"],
  body: [
    "## System",
    ...system(t.app),
    `- Camera: ${t.camera.make} ${t.camera.model}${t.camera.firmware ? `, firmware ${t.camera.firmware}` : ""}`,
    "",
    "## Result",
    fence(t.report, "markdown"),
    footer(ids),
  ].join("\n"),
});
