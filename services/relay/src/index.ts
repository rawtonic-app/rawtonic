// SPDX-License-Identifier: AGPL-3.0-or-later
// Rawtonic's contribution relay (ADR 0017): takes problem reports (with an
// optional screenshot and photo file), crash reports, camera-test results
// and usage statistics from the app, files them privately and deletes them
// on request or after their retention. Deployment: docs/RELAY.md.
//
// It is built for the Workers Free plan's 10 ms of CPU per request (to be
// checked after each deploy, docs/RELAY.md): JSON bodies
// are small, and files arrive as raw bytes that the runtime pipes into R2
// without JavaScript touching them (`PUT /upload/:token`).

import { GitHubTracker, type IssueTracker } from "./github";
import { cameraTestIssue, crashIssue, reportIssue } from "./issues";
import { allow, checkPow, takeGlobal, type Kind } from "./limits";
import {
  BODY_CAPS,
  Invalid,
  MAX_PHOTO_BYTES,
  MAX_SCREENSHOT_BYTES,
  hasHomePath,
  parseCameraTest,
  parseCrash,
  parseReport,
  parseStats,
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
  /** How long an upload's bytes may take (tests shorten it). */
  uploadDeadlineMs?: number;
}

/** An upload must finish within this; it starts at most an hour after its
 * report was filed (UPLOAD_VALID_MS), so after 90 minutes no write to the
 * report's objects can follow (see `stale`). */
const UPLOAD_DEADLINE_MS = 30 * 60_000;

/** Reports (with their files), crashes and camera tests are kept 90 days
 * (ADR 0017). */
const ISSUE_DAYS = 90;
const UPLOAD_VALID_MS = HOUR_MS;
/** Issues one scheduled run deletes by age at most (one list request plus
 * one request each), and records it removes at most (their objects in one
 * bucket call, each record one D1 statement): within the Workers Free
 * plan's 50 subrequests and 50 D1 queries per invocation. The cron runs
 * every 10 minutes (~2,000 records and ~2,800 issues a day). */
const ISSUES_PER_SWEEP = 20;
const RECORDS_PER_SWEEP = 15;
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

/** A screenshot's type, told from its first bytes (PNG signature or JPEG
 * start-of-image marker). */
const looksLike = (head: Uint8Array, mime: string) =>
  mime === "image/png"
    ? [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a].every((b, i) => head[i] === b)
    : head[0] === 0xff && head[1] === 0xd8 && head[2] === 0xff;

const clientIp = (request: Request) => request.headers.get("cf-connecting-ip") ?? "unknown";

const newSubmission = async () => {
  const id = newId();
  const idHash = await sha256Hex(id);
  return { id, idHash, ref: idHash.slice(0, 8) };
};

type Endpoint = "report" | "crash" | "camera-test" | "stats";

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
 * the id is not given out yet), `filed`, and `attaching` (one of a filed
 * report's files is being written; the row counts as `filed` again once
 * the upload's deadline is long over, see `stale`). Removal never changes
 * a row's state: it deletes the external things first and the row last,
 * only if the row is still as it was read, so an interrupted removal
 * leaves the row as it was for the next try. Rows left in `filing` are
 * removed by the scheduled run once their request is long over; an issue
 * GitHub created without answering is not looked for: it goes with the
 * age-based issue retention. */
type State = "filing" | "filed" | "attaching";

interface Row {
  id_hash: string;
  ref: string;
  state: State;
  issue_node_id: string | null;
  objects: string;
  received_at: number;
}

const ROW = "SELECT id_hash, ref, state, issue_node_id, objects, received_at FROM submissions";

/** A row left `attaching` by an upload that stopped half-way: its deadline
 * is long over, so no write to its objects can follow. */
const stale = (row: Row, deps: Deps, now: number) =>
  row.state === "attaching" && now - row.received_at > UPLOAD_VALID_MS + (deps.uploadDeadlineMs ?? UPLOAD_DEADLINE_MS);

/** The files a report may announce; each gets a one-time upload slot. */
type Slot = "screenshot" | "photo";

/** Where a report's file goes: `reports/<id hash>/screenshot.png` or
 * `reports/<id hash>/photo.<file type>`. */
const objectKey = (idHash: string, slot: Slot, ext: string) => `reports/${idHash}/${slot}.${ext}`;
const slotOf = (key: string): Slot => (/\/screenshot\.[a-z]+$/.test(key) ? "screenshot" : "photo");

interface Announced {
  slot: Slot;
  ext: string;
  size: number;
  sha256: string;
}

/** Files one issue, with an upload slot for each announced file: a
 * one-time token, valid an hour, for exactly `size` bytes with this
 * SHA-256. The row is written first, so a failure at any later step leaves
 * something the scheduled run can clean up; the id is only answered once
 * everything (the slots too) is recorded. */
const file = async (
  request: Request,
  env: Env,
  deps: Deps,
  kind: string,
  build: (s: { ref: string; keys: Partial<Record<Slot, string>> }) => Parameters<IssueTracker["create"]>[0],
  announced: Announced[] = [],
) => {
  const now = deps.now();
  if (!(await takeGlobal(env.DB, "issues", 1, now))) return error(429, "busy");
  const s = await newSubmission();
  const row: Row = { id_hash: s.idHash, ref: s.ref, state: "filing", issue_node_id: null, objects: "[]", received_at: now };
  try {
    await env.DB.prepare(
      "INSERT INTO submissions (id_hash, kind, ref, state, objects, received_at) VALUES (?1, ?2, ?3, 'filing', '[]', ?4)",
    )
      .bind(s.idHash, kind, s.ref, now)
      .run();
  } catch {
    return error(502, "not-filed");
  }
  const slots = announced.map((a) => ({ ...a, key: objectKey(s.idHash, a.slot, a.ext), token: newId() + newId() }));
  let issue: { number: number; nodeId: string };
  try {
    issue = await deps.tracker.create(build({ ref: s.ref, keys: Object.fromEntries(slots.map((a) => [a.slot, a.key])) }));
  } catch {
    // GitHub may have created it without answering: the row goes with the
    // scheduled run, the issue (if any) with the age-based issue
    // retention. The sender gets no id for it.
    return error(502, "not-filed");
  }
  const expires = now + UPLOAD_VALID_MS;
  try {
    // One transaction: the slots exist only for a filed row.
    const [filed] = await env.DB.batch([
      env.DB.prepare(
        "UPDATE submissions SET state = 'filed', issue_number = ?2, issue_node_id = ?3 WHERE id_hash = ?1 AND state = 'filing'",
      ).bind(s.idHash, issue.number, issue.nodeId),
      ...(await Promise.all(
        slots.map(async (a) =>
          env.DB.prepare(
            "INSERT INTO uploads (token_hash, id_hash, object_key, size, sha256, meta, expires) VALUES (?1, ?2, ?3, ?4, ?5, '', ?6)",
          ).bind(await sha256Hex(a.token), s.idHash, a.key, a.size, a.sha256, expires),
        ),
      )),
    ]);
    if (filed.meta.changes !== 1) throw new Error("row lost");
  } catch {
    await cleanUp(env, deps, { ...row, issue_node_id: issue.nodeId });
    return error(502, "not-filed");
  }
  if (!slots.length) return json(201, { id: s.id, ref: s.ref });
  const uploads = Object.fromEntries(slots.map((a) => [a.slot, new URL(`/upload/${a.token}`, request.url).toString()]));
  return json(201, { id: s.id, ref: s.ref, uploads, expiresAt: new Date(expires).toISOString() });
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
  const announced: Announced[] = [];
  const shot = report.screenshot;
  if (shot) announced.push({ slot: "screenshot", ext: shot.mime === "image/png" ? "png" : "jpg", size: shot.size, sha256: shot.sha256 });
  const photo = report.photo;
  if (photo?.file) announced.push({ slot: "photo", ext: photo.fileType, ...photo.file });
  return file(request, env, deps, "report", ({ ref, keys }) => reportIssue(report, ref, keys), announced);
};

const postCrash = async (request: Request, env: Env, deps: Deps) => {
  const crash = await accept(request, env, deps, "crash", parseCrash);
  if (crash instanceof Response) return crash;
  return file(request, env, deps, "crash", ({ ref }) => crashIssue(crash, ref));
};

const postCameraTest = async (request: Request, env: Env, deps: Deps) => {
  const test = await accept(request, env, deps, "camera-test", parseCameraTest);
  if (test instanceof Response) return test;
  return file(request, env, deps, "camera-test", ({ ref }) => cameraTestIssue(test, ref));
};

/** Reads the body's first `n` bytes (fewer only if it is shorter) and
 * gives the stream back unlocked, so the rest is piped without JavaScript
 * touching it. */
const peek = async (body: ReadableStream<Uint8Array>, n: number) => {
  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  while (length < n) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    length += value.byteLength;
  }
  reader.releaseLock();
  const head = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    head.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return head;
};

/** `head` and then the rest of the body as one stream of known length for
 * R2, piped by the runtime: no JavaScript touches the chunks, so a large
 * upload should cost little CPU time (Workers Free plan: 10 ms a request;
 * measured after deploy, docs/RELAY.md). */
const joined = (head: Uint8Array, rest: ReadableStream<Uint8Array>, length: number) => {
  const fixed = new FixedLengthStream(length);
  (async () => {
    if (head.byteLength) {
      const writer = fixed.writable.getWriter();
      await writer.write(head);
      writer.releaseLock();
    }
    await rest.pipeTo(fixed.writable);
  })().catch(() => {
    // The put sees the stream fail.
  });
  return fixed.readable;
};

class Deadline extends Error {}

/** `work`, or a `Deadline` failure once `ms` have passed. The runtime's
 * pipe ignores an abort signal, so the deadline fails the request instead;
 * answering ends the request, which should cancel its unfinished put, but
 * a put may still complete after the deadline, so its key stays recorded
 * (see `putUpload`). */
const within = <T>(work: Promise<T>, ms: number) =>
  new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Deadline("upload deadline")), ms);
    work.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (e: unknown) => {
        clearTimeout(timer);
        reject(e);
      },
    );
  });

interface UploadRow {
  token_hash: string;
  id_hash: string;
  object_key: string;
  size: number;
  sha256: string;
  expires: number;
}

/** Takes one announced file of a filed report, once: checks its size, its
 * SHA-256 (R2 verifies it while storing) and, for a screenshot, its type
 * from the first bytes. The report's row is claimed (`attaching`, the file
 * added to its objects) in the same transaction that uses up the token, so
 * a deletion meanwhile waits (409) and an upload while another of the same
 * report runs is refused with its token kept. */
const putUpload = async (request: Request, env: Env, deps: Deps, token: string) => {
  const now = deps.now();
  const tokenHash = await sha256Hex(token);
  const upload = await env.DB.prepare("SELECT * FROM uploads WHERE token_hash = ?1").bind(tokenHash).first<UploadRow>();
  if (!upload || upload.expires < now) return error(404, "unknown-upload");
  const screenshot = slotOf(upload.object_key) === "screenshot";
  const length = Number(request.headers.get("content-length"));
  const max = screenshot ? MAX_SCREENSHOT_BYTES : MAX_PHOTO_BYTES;
  if (!request.body || length !== upload.size || length > max) return error(400, "size-mismatch");
  const mime = upload.object_key.endsWith(".png") ? "image/png" : "image/jpeg";
  const deadline = deps.uploadDeadlineMs ?? UPLOAD_DEADLINE_MS;
  let head = new Uint8Array();
  if (screenshot) {
    try {
      head = await within(peek(request.body, 8), deadline);
    } catch {
      return error(422, "not-stored");
    }
    if (!looksLike(head, mime)) return error(400, "invalid", "screenshot");
    if (upload.expires < deps.now()) return error(404, "unknown-upload");
  }
  const row = await env.DB.prepare(`${ROW} WHERE id_hash = ?1`).bind(upload.id_hash).first<Row>();
  if (!row) return error(404, "unknown-upload");
  if (row.state !== "filed") return error(409, "busy");
  if (!(await takeGlobal(env.DB, "uploadBytes", length, now))) return error(429, "busy");
  const objects = JSON.stringify([...(JSON.parse(row.objects) as string[]), upload.object_key]);
  const [claimed, used] = await env.DB.batch([
    env.DB.prepare("UPDATE submissions SET state = 'attaching', objects = ?3 WHERE id_hash = ?1 AND state = 'filed' AND objects = ?2").bind(
      upload.id_hash,
      row.objects,
      objects,
    ),
    env.DB.prepare(
      "DELETE FROM uploads WHERE token_hash = ?1 AND EXISTS (SELECT 1 FROM submissions WHERE id_hash = ?2 AND state = 'attaching' AND objects = ?3)",
    ).bind(tokenHash, upload.id_hash, objects),
  ]);
  if (claimed.meta.changes !== 1 || used.meta.changes !== 1) return error(409, "busy");
  /** Undoes the claim: the report goes back to filed without this file. */
  const undo = async () => {
    try {
      await env.BUCKET.delete(upload.object_key);
      await env.DB.prepare("UPDATE submissions SET state = 'filed', objects = ?3 WHERE id_hash = ?1 AND state = 'attaching' AND objects = ?2")
        .bind(upload.id_hash, objects, row.objects)
        .run();
    } catch {
      // The row stays `attaching` and becomes `stale`: deletion and
      // retention remove it with its objects.
    }
  };
  try {
    const put = env.BUCKET.put(upload.object_key, joined(head, request.body, length), {
      sha256: upload.sha256,
      httpMetadata: { contentType: screenshot ? mime : "application/octet-stream" },
    });
    await within(put, deadline);
  } catch (e) {
    // A put past its deadline may still complete: its key stays recorded
    // and the row `attaching`, so once `stale` (90 minutes after the
    // report) deletion and retention remove whatever it wrote. A put that
    // failed has stopped: undone at once.
    if (!(e instanceof Deadline)) await undo();
    return error(422, "not-stored");
  }
  try {
    const filed = await env.DB.prepare("UPDATE submissions SET state = 'filed' WHERE id_hash = ?1 AND state = 'attaching' AND objects = ?2")
      .bind(upload.id_hash, objects)
      .run();
    if (filed.meta.changes !== 1) throw new Error("row lost");
  } catch {
    await undo();
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

/** Deletes everything stored for one submission: its recorded issue, its
 * objects, then the row — only if it is still in the state it was read
 * in. False when the row changed meanwhile (an upload started, say); it is
 * then kept. */
const remove = async (env: Env, deps: Deps, row: Row): Promise<boolean> => {
  if (row.issue_node_id) await deps.tracker.delete(row.issue_node_id);
  const objects = JSON.parse(row.objects) as string[];
  if (objects.length) await env.BUCKET.delete(objects);
  const [, deleted] = await env.DB.batch([
    env.DB.prepare("DELETE FROM uploads WHERE id_hash = ?1 AND ?2 IN ('filed', 'attaching')").bind(row.id_hash, row.state),
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
  // An upload still running: its objects are being written.
  if (row.state !== "filed" && !stale(row, deps, deps.now())) return error(409, "busy");
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
    if (path === "/stats") return postStats(request, env, deps);
  }
  const upload = /^\/upload\/([a-z2-7]{52})$/.exec(path);
  if (upload && method === "PUT") return putUpload(request, env, deps, upload[1]);
  const del = /^\/report\/([^/]+)$/.exec(path);
  if (del && method === "DELETE") return deleteSubmission(request, env, deps, del[1]);
  if (path === "/stats/summary" && method === "GET") return statsSummary(request, env, url);
  return error(404, "not-found");
};

/** The sweep's reads of expired records: issue-backed kinds, and filings
 * whose request stopped half-way (?1 = cutoff, ?2 = limit). */
export const SWEEP_QUERIES = [
  `${ROW} WHERE state IN ('filed', 'attaching') AND kind IN ('report', 'crash', 'camera-test') AND received_at < ?1 LIMIT ?2`,
  `${ROW} WHERE state = 'filing' AND kind IN ('report', 'crash', 'camera-test') AND received_at < ?1 LIMIT ?2`,
] as const;

/** The scheduled run, in three independent steps (a failure in one never
 * holds up the others): housekeeping; records and their objects by age
 * (retention, and filings whose request stopped half-way two hours ago),
 * without GitHub; and issues by age, read from the repository itself,
 * oldest first. */
export const sweep = async (env: Env, deps: Deps) => {
  const now = deps.now();
  try {
    await env.DB.batch([
      env.DB.prepare("DELETE FROM rate WHERE expires < ?1").bind(now),
      env.DB.prepare("DELETE FROM pow_seen WHERE expires < ?1").bind(now),
      env.DB.prepare("DELETE FROM uploads WHERE expires < ?1").bind(now),
    ]);
    await closeWeeks(env.DB, now);
  } catch {
    // Tried again next run; retention below does not depend on it.
  }
  try {
    // Range reads on the (state, kind, received_at) index, each bounded,
    // never a scan of the table.
    const [issues, unfinished] = SWEEP_QUERIES;
    const ranges = [
      env.DB.prepare(issues).bind(now - ISSUE_DAYS * DAY_MS, RECORDS_PER_SWEEP),
      env.DB.prepare(unfinished).bind(now - UPLOAD_VALID_MS - HOUR_MS, RECORDS_PER_SWEEP),
    ];
    const results = (await env.DB.batch<Row>(ranges)).flatMap((r) => r.results).slice(0, RECORDS_PER_SWEEP);
    // Objects first (one call), then each record only if unchanged since
    // it was read; their upload tokens, if any, expired long ago.
    const keys = results.flatMap((row) => JSON.parse(row.objects) as string[]);
    if (keys.length) await env.BUCKET.delete(keys);
    if (results.length) {
      await env.DB.batch(
        results.map((row) =>
          env.DB.prepare("DELETE FROM submissions WHERE id_hash = ?1 AND state = ?2 AND objects = ?3").bind(
            row.id_hash,
            row.state,
            row.objects,
          ),
        ),
      );
    }
  } catch {
    // Tried again next run.
  }
  try {
    for (const issue of await deps.tracker.listOldest(ISSUES_PER_SWEEP)) {
      if (issue.createdAt >= now - ISSUE_DAYS * DAY_MS) break;
      await deps.tracker.delete(issue.nodeId);
    }
  } catch {
    // GitHub unreachable or the token expired: tried again next run.
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
