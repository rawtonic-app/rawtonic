// SPDX-License-Identifier: AGPL-3.0-or-later
// Rawtonic's contribution relay (ADR 0017): takes problem reports, crash
// reports, camera-test results, RAW samples and usage statistics from the
// app, files them privately and deletes them on request or after their
// retention. Deployment: docs/RELAY.md.

import { GitHubTracker, type IssueTracker } from "./github";
import { cameraTestIssue, crashIssue, marker, reportIssue, type Ids } from "./issues";
import { allow, checkPow, takeGlobal, type Kind } from "./limits";
import {
  BODY_CAPS,
  Invalid,
  MAX_SAMPLE_BYTES,
  MAX_SCREENSHOT_BYTES,
  hasHomePath,
  parseCameraTest,
  parseCrash,
  parseRawSample,
  parseReport,
  parseStats,
  type RawSample,
} from "./schema";
import { closeWeeks, record, summary, weekOpen } from "./stats";
import { DAY_MS, HOUR_MS, ID_PATTERN, WEEK_PATTERN, newId, sameSecret, sha256Hex } from "./util";

export interface Env {
  DB: D1Database;
  BUCKET: R2Bucket;
  /** Secrets (`wrangler secret put`). */
  GITHUB_TOKEN: string;
  IP_HASH_KEY: string;
  ADMIN_TOKEN: string;
  /** Variables (wrangler.jsonc). */
  GITHUB_REPO: string;
  POW_BITS?: string;
}

export interface Deps {
  tracker: IssueTracker;
  now: () => number;
}

export const RETENTION_DAYS: Record<string, number> = {
  report: 90,
  crash: 90,
  "camera-test": 90,
  "raw-sample": 730,
};
const UPLOAD_VALID_MS = HOUR_MS;
/** What one scheduled run may spend on removals, in GitHub requests: a
 * recorded issue costs 2 (its deletion, the bucket), an unresolved filing
 * up to 12 (a 10-page issue list too). Keeps a run within the Workers
 * Free plan's 50 subrequests and 50 D1 queries per invocation (each
 * removal is one D1 batch); the cron runs every 10 minutes. */
const SWEEP_BUDGET = 36;
const LIST_PAGES = 10;

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const error = (status: number, code: string, field?: string) => json(status, field ? { error: code, field } : { error: code });

/** Reads at most `cap` bytes; null when the body is longer. */
const readCapped = async (request: Request, cap: number): Promise<Uint8Array | null> => {
  const declared = request.headers.get("content-length");
  if (declared !== null && Number(declared) > cap) return null;
  if (!request.body) return new Uint8Array();
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    length += value.byteLength;
    if (length > cap) {
      await reader.cancel();
      return null;
    }
    chunks.push(value);
  }
  const out = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
};

const decodeBase64 = (data: string): Uint8Array => Uint8Array.from(atob(data), (c) => c.charCodeAt(0));

const looksLike = (bytes: Uint8Array, mime: string) =>
  mime === "image/png"
    ? [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a].every((b, i) => bytes[i] === b)
    : bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff;

const clientIp = (request: Request) => request.headers.get("cf-connecting-ip") ?? "unknown";

const newSubmission = async () => {
  const id = newId();
  const idHash = await sha256Hex(id);
  return { id, idHash, ref: idHash.slice(0, 8) };
};

type Endpoint = "report" | "crash" | "camera-test" | "raw-sample" | "stats";

/** Reads, checks and parses a JSON submission; a Response when refused. */
const accept = async <T>(
  request: Request,
  env: Env,
  deps: Deps,
  endpoint: Endpoint,
  parse: (value: unknown) => T,
): Promise<T | Response> => {
  if (!request.headers.get("content-type")?.startsWith("application/json")) return error(415, "json-only");
  if (!request.headers.get("x-rawtonic-client")) return error(400, "client-header");
  const body = await readCapped(request, BODY_CAPS[endpoint]);
  if (body === null) return error(413, "too-large");
  const now = deps.now();
  if (!(await checkPow(env.DB, Number(env.POW_BITS ?? 0), request.headers.get("x-rawtonic-pow"), body, now))) {
    return error(403, "proof-of-work");
  }
  if (!(await allow(env.DB, env.IP_HASH_KEY, endpoint as Kind, clientIp(request), now))) {
    return error(429, "rate-limited");
  }
  let parsed: T;
  try {
    parsed = parse(JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(body)));
  } catch (e) {
    return e instanceof Invalid ? error(400, "invalid", e.field) : error(400, "invalid");
  }
  if (hasHomePath(parsed)) return error(422, "unscrubbed-path");
  return parsed;
};

/** Row states: `filing` (recorded before anything external is written;
 * the id is not given out yet; its issue may exist without its number
 * recorded), `pending` (RAW sample announced), `uploading` (its bytes are
 * being written) and `filed`. Removal never changes a row's state: it
 * deletes the external things first and the row last, only if the row is
 * still as it was read, so an interrupted removal leaves the row as it was
 * for the next try. Rows left in `filing`, `uploading` or `pending` are
 * removed by the scheduled run once their request is long over. */
type State = "filing" | "pending" | "uploading" | "filed";

interface Row {
  id_hash: string;
  ref: string;
  state: State;
  issue_node_id: string | null;
  objects: string;
  received_at: number;
}

const ROW = "SELECT id_hash, ref, state, issue_node_id, objects, received_at FROM submissions";

/** Files one issue (with its stored objects). The row is written first, so
 * a failure at any later step leaves something the scheduled run can clean
 * up; the id is only answered once everything is recorded. */
const file = async (
  env: Env,
  deps: Deps,
  kind: string,
  build: (s: Ids & { keys: string[] }) => Parameters<IssueTracker["create"]>[0],
  objects: { key: string; data: Uint8Array; type: string }[] = [],
) => {
  const now = deps.now();
  if (!(await takeGlobal(env.DB, "issues", 1, now))) return error(429, "busy");
  const s = await newSubmission();
  const keys = objects.map((o) => o.key.replace("{hash}", s.idHash));
  const row: Row = { id_hash: s.idHash, ref: s.ref, state: "filing", issue_node_id: null, objects: JSON.stringify(keys), received_at: now };
  try {
    await env.DB.prepare(
      "INSERT INTO submissions (id_hash, kind, ref, state, objects, received_at) VALUES (?1, ?2, ?3, 'filing', ?4, ?5)",
    )
      .bind(s.idHash, kind, s.ref, row.objects, now)
      .run();
  } catch {
    return error(502, "not-filed");
  }
  try {
    for (const [i, o] of objects.entries()) {
      await env.BUCKET.put(keys[i], o.data, { httpMetadata: { contentType: o.type } });
    }
  } catch {
    await cleanUp(env, deps, row);
    return error(502, "not-filed");
  }
  let issue: { number: number; nodeId: string };
  try {
    issue = await deps.tracker.create(build({ ref: s.ref, idHash: s.idHash, keys }));
  } catch {
    // GitHub may have created it without answering: the row stays in
    // `filing` and the scheduled run looks for the issue's marker later.
    return error(502, "not-filed");
  }
  try {
    const filed = await env.DB.prepare(
      "UPDATE submissions SET state = 'filed', issue_number = ?2, issue_node_id = ?3 WHERE id_hash = ?1 AND state = 'filing'",
    )
      .bind(s.idHash, issue.number, issue.nodeId)
      .run();
    if (filed.meta.changes !== 1) throw new Error("row lost");
  } catch {
    await cleanUp(env, deps, { ...row, issue_node_id: issue.nodeId });
    return error(502, "not-filed");
  }
  return json(201, { id: s.id, ref: s.ref });
};

/** Removes what a failed request wrote. When that fails too, the row stays
 * and the scheduled run tries again. */
const cleanUp = async (env: Env, deps: Deps, row: Row) => {
  try {
    await remove(env, deps, row);
  } catch {
    // Left to sweep().
  }
};

const postReport = async (request: Request, env: Env, deps: Deps) => {
  const report = await accept(request, env, deps, "report", parseReport);
  if (report instanceof Response) return report;
  const objects = [];
  if (report.screenshot) {
    const data = decodeBase64(report.screenshot.data);
    if (data.byteLength > MAX_SCREENSHOT_BYTES || !looksLike(data, report.screenshot.mime)) {
      return error(400, "invalid", "screenshot.data");
    }
    const ext = report.screenshot.mime === "image/png" ? "png" : "jpg";
    objects.push({ key: `screenshots/{hash}.${ext}`, data, type: report.screenshot.mime });
  }
  return file(
    env,
    deps,
    "report",
    (ids) => reportIssue(report, ids, ids.keys[0]),
    objects,
  );
};

const postCrash = async (request: Request, env: Env, deps: Deps) => {
  const crash = await accept(request, env, deps, "crash", parseCrash);
  if (crash instanceof Response) return crash;
  return file(env, deps, "crash", (ids) => crashIssue(crash, ids));
};

const postCameraTest = async (request: Request, env: Env, deps: Deps) => {
  const test = await accept(request, env, deps, "camera-test", parseCameraTest);
  if (test instanceof Response) return test;
  return file(env, deps, "camera-test", (ids) => cameraTestIssue(test, ids));
};

const sampleKey = (idHash: string, s: RawSample) =>
  `raw-samples/${idHash}/${`${s.camera.make}-${s.camera.model}`.replace(/[^A-Za-z0-9._-]+/g, "_")}.${s.fileType}`;

/** Announces a RAW sample: answers a one-time upload URL. */
const postRawSample = async (request: Request, env: Env, deps: Deps) => {
  const sample = await accept(request, env, deps, "raw-sample", parseRawSample);
  if (sample instanceof Response) return sample;
  const now = deps.now();
  if (!(await takeGlobal(env.DB, "samples", 1, now))) return error(429, "busy");
  const s = await newSubmission();
  const token = newId() + newId();
  const key = sampleKey(s.idHash, sample);
  const expires = now + UPLOAD_VALID_MS;
  await env.DB.batch([
    env.DB.prepare(
      "INSERT INTO uploads (token_hash, id_hash, object_key, size, sha256, meta, expires) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    ).bind(await sha256Hex(token), s.idHash, key, sample.size, sample.sha256, JSON.stringify(sample), expires),
    env.DB.prepare(
      "INSERT INTO submissions (id_hash, kind, ref, state, objects, received_at) VALUES (?1, 'raw-sample', ?2, 'pending', '[]', ?3)",
    ).bind(s.idHash, s.ref, now),
  ]);
  const uploadUrl = new URL(`/raw-sample/upload/${token}`, request.url).toString();
  return json(201, { id: s.id, ref: s.ref, uploadUrl, expiresAt: new Date(expires).toISOString() });
};

/** Takes a RAW sample's bytes once and checks size and SHA-256. Samples
 * are not filed as issues: the maintainers list them from the database
 * and the bucket (docs/RELAY.md), so no GitHub write can be left behind. */
const putRawSample = async (request: Request, env: Env, deps: Deps, token: string) => {
  const now = deps.now();
  const upload = await env.DB.prepare("DELETE FROM uploads WHERE token_hash = ?1 RETURNING *")
    .bind(await sha256Hex(token))
    .first<{ id_hash: string; object_key: string; size: number; sha256: string; meta: string; expires: number }>();
  if (!upload || upload.expires < now) return error(404, "unknown-upload");
  const length = Number(request.headers.get("content-length"));
  if (!request.body || length !== upload.size || length > MAX_SAMPLE_BYTES) return error(400, "size-mismatch");
  if (!(await takeGlobal(env.DB, "uploadBytes", length, now))) return error(429, "busy");
  // Claimed before anything is written, with the objects recorded: a
  // deletion now waits (409) instead of racing the upload. Beside the file
  // goes what the donor announced (camera, licence, note) as sample.json.
  const metaKey = upload.object_key.replace(/[^/]+$/, "sample.json");
  const objects = JSON.stringify([upload.object_key, metaKey]);
  const claimed = await env.DB.prepare(
    "UPDATE submissions SET state = 'uploading', objects = ?2 WHERE id_hash = ?1 AND state = 'pending'",
  )
    .bind(upload.id_hash, objects)
    .run();
  if (claimed.meta.changes !== 1) return error(404, "unknown-upload");
  const row: Row = { id_hash: upload.id_hash, ref: upload.id_hash.slice(0, 8), state: "uploading", issue_node_id: null, objects, received_at: now };
  try {
    await env.BUCKET.put(upload.object_key, request.body, { sha256: upload.sha256 });
    await env.BUCKET.put(metaKey, upload.meta, { httpMetadata: { contentType: "application/json" } });
  } catch {
    await cleanUp(env, deps, row);
    return error(422, "checksum-mismatch");
  }
  try {
    const filed = await env.DB.prepare("UPDATE submissions SET state = 'filed' WHERE id_hash = ?1 AND state = 'uploading'")
      .bind(upload.id_hash)
      .run();
    if (filed.meta.changes !== 1) throw new Error("row lost");
  } catch {
    await cleanUp(env, deps, row);
    return error(502, "not-filed");
  }
  return new Response(null, { status: 204 });
};

const postStats = async (request: Request, env: Env, deps: Deps) => {
  const stats = await accept(request, env, deps, "stats", parseStats);
  if (stats instanceof Response) return stats;
  if (!weekOpen(stats.week, deps.now())) return error(400, "invalid", "week");
  await record(env.DB, stats);
  return new Response(null, { status: 204 });
};

/** Deletes everything stored for one submission: its issue (a `filing`
 * row without a recorded number: every issue carrying its marker, from the
 * consistent issue list; an unreadable list throws), its objects, then the
 * row — only if it is still in the state it was read in. False when the
 * row changed meanwhile (an upload started, say); it is then kept. */
const remove = async (env: Env, deps: Deps, row: Row): Promise<boolean> => {
  const issues = row.issue_node_id
    ? [row.issue_node_id]
    : row.state === "filing"
      ? await deps.tracker.findByMarker(marker(row.id_hash), row.received_at - 10 * 60_000, LIST_PAGES)
      : [];
  for (const nodeId of issues) await deps.tracker.delete(nodeId);
  const objects = JSON.parse(row.objects) as string[];
  if (objects.length) await env.BUCKET.delete(objects);
  const [, deleted] = await env.DB.batch([
    env.DB.prepare("DELETE FROM uploads WHERE id_hash = ?1 AND ?2 IN ('pending', 'filed')").bind(row.id_hash, row.state),
    env.DB.prepare("DELETE FROM submissions WHERE id_hash = ?1 AND state = ?2 AND objects = ?3").bind(
      row.id_hash,
      row.state,
      row.objects,
    ),
  ]);
  return deleted.meta.changes === 1;
};

const deleteSubmission = async (request: Request, env: Env, deps: Deps, id: string) => {
  if (!(await allow(env.DB, env.IP_HASH_KEY, "delete", clientIp(request), deps.now()))) {
    return error(429, "rate-limited");
  }
  if (!ID_PATTERN.test(id)) return error(404, "unknown-id");
  const row = await env.DB.prepare(`${ROW} WHERE id_hash = ?1`).bind(await sha256Hex(id)).first<Row>();
  if (!row) return error(404, "unknown-id");
  // A RAW sample still uploading: its objects are being written.
  if (row.state !== "filed" && row.state !== "pending") return error(409, "busy");
  try {
    return (await remove(env, deps, row)) ? new Response(null, { status: 204 }) : error(409, "busy");
  } catch {
    return error(502, "not-deleted");
  }
};

const statsSummary = async (request: Request, env: Env, url: URL) => {
  const auth = request.headers.get("authorization") ?? "";
  if (!env.ADMIN_TOKEN || !sameSecret(auth, `Bearer ${env.ADMIN_TOKEN}`)) return error(401, "unauthorized");
  const week = url.searchParams.get("week") ?? "";
  if (!WEEK_PATTERN.test(week)) return error(400, "invalid", "week");
  return json(200, { week, counts: await summary(env.DB, week) });
};

export const handle = async (request: Request, env: Env, deps: Deps): Promise<Response> => {
  const url = new URL(request.url);
  const path = url.pathname;
  const method = request.method;
  if (method === "POST") {
    if (path === "/report") return postReport(request, env, deps);
    if (path === "/crash") return postCrash(request, env, deps);
    if (path === "/camera-test") return postCameraTest(request, env, deps);
    if (path === "/raw-sample") return postRawSample(request, env, deps);
    if (path === "/stats") return postStats(request, env, deps);
  }
  const upload = /^\/raw-sample\/upload\/([a-z2-7]{52})$/.exec(path);
  if (upload && method === "PUT") return putRawSample(request, env, deps, upload[1]);
  const del = /^\/report\/([^/]+)$/.exec(path);
  if (del && method === "DELETE") return deleteSubmission(request, env, deps, del[1]);
  if (path === "/stats/summary" && method === "GET") return statsSummary(request, env, url);
  return error(404, "not-found");
};

/** The scheduled run: housekeeping first (so it always runs), then as many
 * removals as its budget allows — retention, then rows whose request
 * stopped half-way (two hours after it began). */
export const sweep = async (env: Env, deps: Deps) => {
  const now = deps.now();
  await env.DB.batch([
    env.DB.prepare("DELETE FROM rate WHERE expires < ?1").bind(now),
    env.DB.prepare("DELETE FROM pow_seen WHERE expires < ?1").bind(now),
    env.DB.prepare("DELETE FROM uploads WHERE expires < ?1").bind(now),
  ]);
  await closeWeeks(env.DB, now);
  const due: Row[] = [];
  for (const [kind, days] of Object.entries(RETENTION_DAYS)) {
    const { results } = await env.DB.prepare(`${ROW} WHERE kind = ?1 AND state = 'filed' AND received_at < ?2 LIMIT 18`)
      .bind(kind, now - days * DAY_MS)
      .all<Row>();
    due.push(...results);
  }
  const { results: stale } = await env.DB.prepare(
    `${ROW} WHERE state IN ('pending', 'filing', 'uploading') AND received_at < ?1 LIMIT 18`,
  )
    .bind(now - UPLOAD_VALID_MS - HOUR_MS)
    .all<Row>();
  due.push(...stale);
  let budget = SWEEP_BUDGET;
  for (const row of due) {
    const cost = row.state === "filing" && !row.issue_node_id ? 2 + LIST_PAGES : 2;
    if (cost > budget) continue;
    budget -= cost;
    try {
      await remove(env, deps, row);
    } catch {
      // Tried again next run.
    }
  }
};

const deps = (env: Env): Deps => ({
  tracker: new GitHubTracker(env.GITHUB_TOKEN, env.GITHUB_REPO),
  now: () => Date.now(),
});

export default {
  fetch: (request, env) => handle(request, env, deps(env)),
  scheduled: (_controller, env, ctx) => ctx.waitUntil(sweep(env, deps(env))),
} satisfies ExportedHandler<Env>;
