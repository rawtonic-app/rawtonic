// SPDX-License-Identifier: AGPL-3.0-or-later
// Rawtonic's contribution relay (ADR 0017): takes problem reports, crash
// reports, camera-test results, RAW samples and usage statistics from the
// app, files them privately and deletes them on request or after their
// retention. Deployment: docs/RELAY.md.

import { GitHubTracker, type IssueTracker } from "./github";
import { cameraTestIssue, crashIssue, rawSampleIssue, reportIssue } from "./issues";
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
/** Deletions per scheduled run (each is one GitHub request). */
const RETENTION_BATCH = 40;

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

const insertSubmission = (
  env: Env,
  s: { idHash: string; ref: string },
  kind: string,
  state: "pending" | "filed",
  objects: string[],
  now: number,
  issue?: { number: number; nodeId: string },
) =>
  env.DB.prepare(
    "INSERT INTO submissions (id_hash, kind, ref, state, issue_number, issue_node_id, objects, received_at) " +
      "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
  )
    .bind(s.idHash, kind, s.ref, state, issue?.number ?? null, issue?.nodeId ?? null, JSON.stringify(objects), now)
    .run();

/** Files one issue (with its stored objects) and records it; on failure
 * removes the objects again. */
const file = async (
  env: Env,
  deps: Deps,
  kind: string,
  build: (s: { ref: string; keys: string[] }) => Parameters<IssueTracker["create"]>[0],
  objects: { key: string; data: Uint8Array; type: string }[] = [],
) => {
  const now = deps.now();
  if (!(await takeGlobal(env.DB, "issues", 1, now))) return error(429, "busy");
  const s = await newSubmission();
  const keys = objects.map((o) => o.key.replace("{hash}", s.idHash));
  try {
    for (const [i, o] of objects.entries()) {
      await env.BUCKET.put(keys[i], o.data, { httpMetadata: { contentType: o.type } });
    }
    const issue = await deps.tracker.create(build({ ref: s.ref, keys }));
    await insertSubmission(env, s, kind, "filed", keys, now, issue);
  } catch {
    if (keys.length) await env.BUCKET.delete(keys);
    return error(502, "not-filed");
  }
  return json(201, { id: s.id, ref: s.ref });
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
    ({ ref, keys }) => reportIssue(report, ref, keys[0]),
    objects,
  );
};

const postCrash = async (request: Request, env: Env, deps: Deps) => {
  const crash = await accept(request, env, deps, "crash", parseCrash);
  if (crash instanceof Response) return crash;
  return file(env, deps, "crash", ({ ref }) => crashIssue(crash, ref));
};

const postCameraTest = async (request: Request, env: Env, deps: Deps) => {
  const test = await accept(request, env, deps, "camera-test", parseCameraTest);
  if (test instanceof Response) return test;
  return file(env, deps, "camera-test", ({ ref }) => cameraTestIssue(test, ref));
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

/** Takes a RAW sample's bytes once, checks size and SHA-256, files the issue. */
const putRawSample = async (request: Request, env: Env, deps: Deps, token: string) => {
  const now = deps.now();
  const upload = await env.DB.prepare("DELETE FROM uploads WHERE token_hash = ?1 RETURNING *")
    .bind(await sha256Hex(token))
    .first<{ id_hash: string; object_key: string; size: number; sha256: string; meta: string; expires: number }>();
  if (!upload || upload.expires < now) return error(404, "unknown-upload");
  const length = Number(request.headers.get("content-length"));
  if (!request.body || length !== upload.size || length > MAX_SAMPLE_BYTES) return error(400, "size-mismatch");
  if (!(await takeGlobal(env.DB, "uploadBytes", length, now))) return error(429, "busy");
  const sample = JSON.parse(upload.meta) as RawSample;
  try {
    await env.BUCKET.put(upload.object_key, request.body, { sha256: upload.sha256 });
  } catch {
    await env.BUCKET.delete(upload.object_key);
    return error(422, "checksum-mismatch");
  }
  const ref = upload.id_hash.slice(0, 8);
  try {
    if (!(await takeGlobal(env.DB, "issues", 1, now))) throw new Error("issue cap");
    const issue = await deps.tracker.create(rawSampleIssue(sample, ref, upload.object_key));
    const updated = await env.DB.prepare(
      "UPDATE submissions SET state = 'filed', issue_number = ?2, issue_node_id = ?3, objects = ?4 WHERE id_hash = ?1",
    )
      .bind(upload.id_hash, issue.number, issue.nodeId, JSON.stringify([upload.object_key]))
      .run();
    if (updated.meta.changes === 0) {
      // Deleted by its id while it was uploading.
      await remove(env, deps, { id_hash: upload.id_hash, issue_node_id: issue.nodeId, objects: JSON.stringify([upload.object_key]) });
      return error(404, "unknown-upload");
    }
  } catch {
    // Keep the sample (it was announced and paid for in limits); record its
    // object so retention or deletion removes it. The maintainers find it
    // in the bucket.
    const kept = await env.DB.prepare("UPDATE submissions SET state = 'filed', objects = ?2 WHERE id_hash = ?1")
      .bind(upload.id_hash, JSON.stringify([upload.object_key]))
      .run();
    if (kept.meta.changes === 0) await env.BUCKET.delete(upload.object_key);
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

/** Deletes everything stored for one submission. */
const remove = async (
  env: Env,
  deps: Deps,
  row: { id_hash: string; issue_node_id: string | null; objects: string },
) => {
  if (row.issue_node_id) await deps.tracker.delete(row.issue_node_id);
  const objects = JSON.parse(row.objects) as string[];
  if (objects.length) await env.BUCKET.delete(objects);
  await env.DB.batch([
    env.DB.prepare("DELETE FROM uploads WHERE id_hash = ?1").bind(row.id_hash),
    env.DB.prepare("DELETE FROM submissions WHERE id_hash = ?1").bind(row.id_hash),
  ]);
};

const deleteSubmission = async (request: Request, env: Env, deps: Deps, id: string) => {
  if (!(await allow(env.DB, env.IP_HASH_KEY, "delete", clientIp(request), deps.now()))) {
    return error(429, "rate-limited");
  }
  if (!ID_PATTERN.test(id)) return error(404, "unknown-id");
  const row = await env.DB.prepare("SELECT id_hash, issue_node_id, objects FROM submissions WHERE id_hash = ?1")
    .bind(await sha256Hex(id))
    .first<{ id_hash: string; issue_node_id: string | null; objects: string }>();
  if (!row) return error(404, "unknown-id");
  try {
    await remove(env, deps, row);
  } catch {
    return error(502, "not-deleted");
  }
  return new Response(null, { status: 204 });
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

/** The scheduled run: retention, expired limits and tokens, statistics weeks. */
export const sweep = async (env: Env, deps: Deps) => {
  const now = deps.now();
  let budget = RETENTION_BATCH;
  for (const [kind, days] of Object.entries(RETENTION_DAYS)) {
    if (budget <= 0) break;
    const { results } = await env.DB.prepare(
      "SELECT id_hash, issue_node_id, objects FROM submissions WHERE kind = ?1 AND received_at < ?2 LIMIT ?3",
    )
      .bind(kind, now - days * DAY_MS, budget)
      .all<{ id_hash: string; issue_node_id: string | null; objects: string }>();
    for (const row of results) {
      budget--;
      try {
        await remove(env, deps, row);
      } catch {
        // Tried again next run.
      }
    }
  }
  // Announced samples never uploaded (their token expired).
  const { results: stale } = await env.DB.prepare(
    "SELECT id_hash, issue_node_id, objects FROM submissions WHERE state = 'pending' AND received_at < ?1",
  )
    .bind(now - UPLOAD_VALID_MS - HOUR_MS)
    .all<{ id_hash: string; issue_node_id: string | null; objects: string }>();
  for (const row of stale) await remove(env, deps, row);
  await env.DB.batch([
    env.DB.prepare("DELETE FROM rate WHERE expires < ?1").bind(now),
    env.DB.prepare("DELETE FROM pow_seen WHERE expires < ?1").bind(now),
    env.DB.prepare("DELETE FROM uploads WHERE expires < ?1").bind(now),
  ]);
  await closeWeeks(env.DB, now);
};

const deps = (env: Env): Deps => ({
  tracker: new GitHubTracker(env.GITHUB_TOKEN, env.GITHUB_REPO),
  now: () => Date.now(),
});

export default {
  fetch: (request, env) => handle(request, env, deps(env)),
  scheduled: (_controller, env, ctx) => ctx.waitUntil(sweep(env, deps(env))),
} satisfies ExportedHandler<Env>;
