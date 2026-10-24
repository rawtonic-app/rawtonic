// SPDX-License-Identifier: AGPL-3.0-or-later
// The issue text for each kind of submission. User text and logs go into
// fenced blocks only, so they cannot mention people or add markup; titles
// hold only validated names and versions.

import type { App, CameraTest, Crash, Gpu, RawSample, Report } from "./schema";
import type { NewIssue } from "./github";
import { fence } from "./util";

const system = (app: App, gpu?: Gpu) =>
  [
    `- Rawtonic ${app.version}, ${app.os} ${app.osVersion} (${app.arch})`,
    gpu ? `- GPU: ${gpu.vendor}${gpu.name ? ` — ${gpu.name}` : ""}` : null,
  ].filter((line) => line !== null);

const footer = (ref: string, retention: string) =>
  `\n---\nReference ${ref}. Filed by the Rawtonic relay (ADR 0017); deleted after ${retention} or when the sender deletes it by its id.`;

export const reportIssue = (r: Report, ref: string, screenshotKey?: string): NewIssue => ({
  title: `Problem report ${ref} · Rawtonic ${r.app.version} · ${r.app.os}`,
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
    footer(ref, "90 days"),
  ]
    .filter((line) => line !== null)
    .join("\n"),
});

export const crashIssue = (c: Crash, ref: string): NewIssue => ({
  title: `Crash ${ref} · Rawtonic ${c.app.version} · ${c.app.os}`,
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
    footer(ref, "90 days"),
  ]
    .filter((line) => line !== null)
    .join("\n"),
});

export const cameraTestIssue = (t: CameraTest, ref: string): NewIssue => ({
  title: `Camera test ${ref} · ${t.camera.make} ${t.camera.model}`,
  labels: ["camera-test"],
  body: [
    "## System",
    ...system(t.app),
    `- Camera: ${t.camera.make} ${t.camera.model}${t.camera.firmware ? `, firmware ${t.camera.firmware}` : ""}`,
    "",
    "## Result",
    fence(t.report, "markdown"),
    footer(ref, "90 days"),
  ].join("\n"),
});

export const rawSampleIssue = (s: RawSample, ref: string, key: string): NewIssue => ({
  title: `RAW sample ${ref} · ${s.camera.make} ${s.camera.model}`,
  labels: ["raw-sample"],
  body: [
    "## Sample",
    `- Camera: ${s.camera.make} ${s.camera.model}`,
    `- File: ${s.fileType}, ${s.size} bytes, SHA-256 \`${s.sha256}\``,
    `- Stored: R2 \`${key}\``,
    `- Licence: ${s.licence} (personal-data notice acknowledged)`,
    ...system(s.app),
    s.note ? `\n## Note\n${fence(s.note)}` : null,
    footer(ref, "two years"),
  ]
    .filter((line) => line !== null)
    .join("\n"),
});
