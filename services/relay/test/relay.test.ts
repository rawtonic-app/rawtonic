// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { SWEEP_QUERIES, handle, sweep } from "../src/index";
import { powHash } from "../src/limits";
import { DAY_MS, sha256Hex } from "../src/util";
import type { Deps, Env } from "../src/index";
import { APP, del, failingDb, makeDeps, migrate, post, testEnv as env } from "./helpers";

const PNG = Uint8Array.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3]);

const report = (extra: Record<string, unknown> = {}) => ({ schema: 2, app: APP, text: "Export stops at 40 %", ...extra });
const shot = async (bytes: Uint8Array = PNG) => ({ mime: "image/png", size: bytes.byteLength, sha256: await sha256Hex(bytes) });
interface Answer {
  id: string;
  ref: string;
  uploads: { screenshot?: string; photo?: string };
  expiresAt: string;
}
const count = async (table: string) => (await env.DB.prepare(`SELECT COUNT(*) AS n FROM ${table}`).first<{ n: number }>())!.n;

const put = (url: string, deps: Deps, body: Uint8Array | ReadableStream<Uint8Array>, length: number, e: Env = env) =>
  handle(new Request(url, { method: "PUT", headers: { "content-length": String(length) }, body }), e, deps);

beforeEach(async () => {
  await migrate();
});

describe("reports", () => {
  it("files a report as a private issue, takes its screenshot as raw bytes, and deletes both by the id", async () => {
    const deps = makeDeps();
    const response = await post("/report", report({ screenshot: await shot() }), deps);
    expect(response.status).toBe(201);
    const { id, ref, uploads } = (await response.json()) as Answer;
    const uploadUrl = uploads.screenshot!;
    expect(uploadUrl).toMatch(/^https:\/\/relay\.test\/upload\/[a-z2-7]{52}$/);
    expect((await put(uploadUrl, deps, PNG, PNG.byteLength)).status).toBe(204);
    expect((await put(uploadUrl, deps, PNG, PNG.byteLength)).status).toBe(404);
    expect(id).toMatch(/^[a-z2-7]{26}$/);
    expect(ref).toBe((await sha256Hex(id)).slice(0, 8));
    const [issue] = deps.tracker.issues.values();
    expect(issue.labels).toEqual(["report"]);
    expect(issue.title).toBe(`Problem report ${ref} · Rawtonic 0.1.0-alpha.4 · macos`);
    expect(issue.body).toContain("```text\nExport stops at 40 %\n```");
    // The id itself is stored nowhere.
    expect(issue.body).not.toContain(id);
    const rows = await env.DB.prepare("SELECT * FROM submissions").all();
    expect(JSON.stringify(rows.results)).not.toContain(id);
    const objects = await env.BUCKET.list({ prefix: "reports/" });
    expect(objects.objects).toHaveLength(1);
    expect(issue.body).toContain(objects.objects[0].key);
    const stored = await env.BUCKET.get(objects.objects[0].key);
    expect(stored?.httpMetadata?.contentType).toBe("image/png");
    expect(new Uint8Array(await stored!.arrayBuffer())).toEqual(PNG);

    expect((await del(id, deps)).status).toBe(204);
    expect(deps.tracker.issues.size).toBe(0);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    expect((await del(id, deps)).status).toBe(404);
  });

  it("refuses bodies that still hold a home-folder path", async () => {
    const deps = makeDeps();
    const response = await post("/report", report({ logTail: "open /Users/anna/Pictures/a.CR3 failed" }), deps);
    expect(response.status).toBe(422);
    expect(deps.tracker.issues.size).toBe(0);
  });

  it("checks content type, client header and size, and takes no JSON screenshot", async () => {
    const deps = makeDeps();
    expect((await post("/report", report(), deps, { headers: { "content-type": "text/plain" } })).status).toBe(415);
    expect((await post("/report", report(), deps, { headers: { "x-rawtonic-client": "" } })).status).toBe(400);
    expect((await post("/crash", { ...report(), text: "a".repeat(100 * 1024) }, deps)).status).toBe(413);
    // JSON stays small: at most 96 KiB per report, never an image inside.
    expect((await post("/report", report({ logTail: "a".repeat(97 * 1024) }), deps, { ip: "198.51.100.9" })).status).toBe(413);
    const inline = await post("/report", report({ screenshot: { mime: "image/png", data: "iVBORw0KGgo=" } }), deps);
    expect(inline.status).toBe(400);
    expect(deps.tracker.issues.size).toBe(0);
  });

  it("refuses screenshot bytes that are no image, of another size or SHA-256, and keeps the report", async () => {
    const deps = makeDeps();
    const fake = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9]);
    const a = (await (await post("/report", report({ screenshot: await shot(fake) }), deps)).json()) as Answer;
    expect((await put(a.uploads.screenshot!, deps, fake, fake.byteLength)).status).toBe(400);
    const b = (await (await post("/report", report({ screenshot: await shot() }), deps)).json()) as Answer;
    expect((await put(b.uploads.screenshot!, deps, PNG.slice(0, 9), 9)).status).toBe(400);
    const wrongHash = { ...(await shot()), sha256: "0".repeat(64) };
    const c = (await (await post("/report", report({ screenshot: wrongHash }), deps)).json()) as Answer;
    expect((await put(c.uploads.screenshot!, deps, PNG, PNG.byteLength)).status).toBe(422);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    const rows = await env.DB.prepare("SELECT state, objects FROM submissions").all<{ state: string; objects: string }>();
    expect(rows.results).toEqual([1, 2, 3].map(() => ({ state: "filed", objects: "[]" })));
    expect(deps.tracker.issues.size).toBe(3);
    expect((await del(a.id, deps)).status).toBe(204);
    expect((await del(c.id, deps)).status).toBe(204);
  });

  it("makes a deletion wait while the screenshot is written, and an upload fail once the report is deleted", async () => {
    const deps = makeDeps();
    const a = (await (await post("/report", report({ screenshot: await shot() }), deps)).json()) as Answer;
    await env.DB.prepare("UPDATE submissions SET state = 'attaching'").run();
    expect((await del(a.id, deps)).status).toBe(409);
    // An upload that stopped half-way is over after its deadline: the
    // report can be deleted again.
    deps.at += 2 * 3_600_000;
    expect((await del(a.id, deps)).status).toBe(204);
    const b = (await (await post("/report", report({ screenshot: await shot() }), deps)).json()) as Answer;
    expect((await del(b.id, deps)).status).toBe(204);
    expect((await put(b.uploads.screenshot!, deps, PNG, PNG.byteLength)).status).toBe(404);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    expect(await count("uploads")).toBe(0);
  });

  it("stores no screenshot when its bytes do not arrive in time, and keeps the report", async () => {
    const deps = { ...makeDeps(), uploadDeadlineMs: 50 };
    const a = (await (await post("/report", report({ screenshot: await shot() }), deps)).json()) as Answer;
    const stalled = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(PNG.slice(0, 9)); // and nothing more
      },
    });
    expect((await put(a.uploads.screenshot!, deps, stalled, PNG.byteLength)).status).toBe(422);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    expect(await env.DB.prepare("SELECT state, objects FROM submissions").first()).toEqual({ state: "filed", objects: "[]" });
    expect((await del(a.id, deps)).status).toBe(204);
  });

  it("rate-limits per IP: five reports an hour", async () => {
    const deps = makeDeps();
    for (let i = 0; i < 5; i++) expect((await post("/report", report(), deps)).status).toBe(201);
    expect((await post("/report", report(), deps)).status).toBe(429);
    expect((await post("/report", report(), deps, { ip: "198.51.100.1" })).status).toBe(201);
    deps.at += 3_600_000;
    expect((await post("/report", report(), deps)).status).toBe(201);
    // The address is stored only as a keyed hash.
    const keys = await env.DB.prepare("SELECT key FROM rate").all<{ key: string }>();
    expect(JSON.stringify(keys.results)).not.toContain("203.0.113.7");
  });

  it("gives no upload slot and removes the record on the scheduled run when GitHub fails", async () => {
    const deps = makeDeps();
    deps.tracker.failCreate = true;
    const response = await post("/report", report({ screenshot: await shot() }), deps);
    expect(response.status).toBe(502);
    expect(await count("uploads")).toBe(0);
    deps.at += 3 * 3_600_000;
    await sweep(env, deps);
    expect(await count("submissions")).toBe(0);
  });

  it("keeps the record when GitHub cannot delete, so the user can retry", async () => {
    const deps = makeDeps();
    const { id } = (await (await post("/crash", { schema: 1, app: APP, message: "boom", location: "src/x.rs:1:1", crashedOn: "2026-10-24" }, deps)).json()) as { id: string };
    deps.tracker.failDelete = true;
    expect((await del(id, deps)).status).toBe(502);
    deps.tracker.failDelete = false;
    expect((await del(id, deps)).status).toBe(204);
  });

  it("leaves no issue behind when recording it fails", async () => {
    const deps = makeDeps();
    const broken = failingDb(/SET state = 'filed'/);
    const response = await post("/report", report({ screenshot: await shot() }), deps, {}, broken);
    expect(response.status).toBe(502);
    expect(deps.tracker.issues.size).toBe(0);
    expect(await count("uploads")).toBe(0);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
  });

  it("leaves an issue it could not remove to the age-based retention", async () => {
    const deps = makeDeps();
    deps.tracker.failDelete = true;
    const response = await post("/report", report(), deps, {}, failingDb(/SET state = 'filed'/));
    expect(response.status).toBe(502);
    expect(deps.tracker.issues.size).toBe(1);
    deps.tracker.failDelete = false;
    deps.at += 3 * 3_600_000;
    await sweep(env, deps);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
    expect(deps.tracker.issues.size).toBe(1);
    deps.at += 90 * DAY_MS;
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(0);
  });

  it("removes the record of a filing GitHub did not answer, and its issue with retention", async () => {
    const deps = makeDeps();
    deps.tracker.createThenThrow = true;
    const response = await post("/report", report({ screenshot: await shot() }), deps);
    expect(response.status).toBe(502);
    expect(deps.tracker.issues.size).toBe(1);
    deps.at += 3_600_000;
    await sweep(env, deps);
    expect(await count("submissions")).toBe(1); // its request may still be running
    deps.at += 2 * 3_600_000;
    await sweep(env, deps);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
    expect(deps.tracker.issues.size).toBe(1);
    deps.at += 90 * DAY_MS;
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(0);
  });

  it("removes old issues and records even when housekeeping fails", async () => {
    const deps = makeDeps();
    await post("/report", report(), deps);
    deps.at += 91 * DAY_MS;
    await sweep(failingDb(/DELETE FROM rate|SELECT DISTINCT week/), deps);
    expect(deps.tracker.issues.size).toBe(0);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
  });

  it("keeps removing old records and their files while GitHub fails", async () => {
    const deps = makeDeps();
    const bytes = new TextEncoder().encode("photo");
    const answer = (await (
      await post("/report", report({ photo: { make: "Sony", model: "ILCE-7M3", fileType: "ARW", file: { size: 5, sha256: await sha256Hex(bytes) } } }), deps)
    ).json()) as Answer;
    expect((await put(answer.uploads.photo!, deps, bytes, 5)).status).toBe(204);
    deps.tracker.failDelete = true;
    deps.tracker.failList = true;
    deps.at += 91 * DAY_MS;
    await sweep(env, deps);
    expect(await count("submissions")).toBe(0);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    expect(deps.tracker.issues.size).toBe(1);
    deps.tracker.failDelete = false;
    deps.tracker.failList = false;
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(0);
  });

  it("files the largest accepted crash within GitHub's issue body limit", async () => {
    const deps = makeDeps();
    const big = "`".repeat(24 * 1024);
    const response = await post(
      "/crash",
      { schema: 1, app: APP, message: "m".repeat(2000), location: "l".repeat(300), backtrace: big, logTail: big, crashedOn: "2026-10-24" },
      deps,
    );
    expect(response.status).toBe(201);
    const [issue] = deps.tracker.issues.values();
    expect(issue.body.length).toBeLessThanOrEqual(65_536);
    const over = await post(
      "/crash",
      { schema: 1, app: APP, message: "m", location: "l", backtrace: `${big}x`, crashedOn: "2026-10-24" },
      deps,
    );
    expect(over.status).toBe(400);
  });

  it("reads expired records through the index, never by scanning the table", async () => {
    for (const sql of SWEEP_QUERIES) {
      const { results } = await env.DB.prepare(`EXPLAIN QUERY PLAN ${sql}`).bind(1, 15).all<{ detail: string }>();
      const plan = results.map((r) => r.detail).join("; ");
      expect(plan).toMatch(/USING INDEX submissions_by_state_age \(state=\? AND kind=\? AND received_at<\?\)/);
      expect(plan).not.toMatch(/SCAN submissions(?! USING)/);
    }
  });

  it("does housekeeping first and removes old reports in budgeted batches", async () => {
    const deps = makeDeps();
    for (let i = 0; i < 25; i++) await post("/report", report(), deps, { ip: `198.51.100.${i}` });
    deps.at += 91 * DAY_MS;
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(25 - 20);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(25 - 15);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM rate").first<{ n: number }>())!.n).toBe(0);
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(0);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
  });

  it("never deletes another submission's issue that shares the short reference", async () => {
    const deps = makeDeps();
    const { ref } = (await (await post("/report", report(), deps)).json()) as { ref: string };
    await env.DB.prepare(
      "INSERT INTO submissions (id_hash, kind, ref, state, objects, received_at) VALUES (?1, 'report', ?2, 'filing', '[]', ?3)",
    )
      .bind(`${ref}${"0".repeat(56)}`, ref, deps.at)
      .run();
    deps.at += 3 * 3_600_000;
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(1);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(1);
  });

  it("files camera tests with the camera in the title", async () => {
    const deps = makeDeps();
    const response = await post(
      "/camera-test",
      { schema: 1, app: APP, camera: { make: "Sony", model: "ILCE-7M3", firmware: "4.01" }, report: "## Camera test\n- connect: passed" },
      deps,
    );
    expect(response.status).toBe(201);
    const [issue] = deps.tracker.issues.values();
    expect(issue.title).toMatch(/^Camera test [0-9a-f]{8} · Sony ILCE-7M3$/);
    expect(issue.labels).toEqual(["camera-test"]);
  });
});

describe("proof of work", () => {
  const stamp = async (body: string, bits: number, seconds: number) => {
    const bodyHash = await sha256Hex(body);
    for (let nonce = 0; ; nonce++) {
      const digest = await powHash(String(seconds), bodyHash, String(nonce));
      if (parseInt(digest.slice(0, 2), 16) >> (8 - bits) === 0) return `${seconds}.${nonce}`;
    }
  };

  it("is enforced when POW_BITS is set, and a stamp works once", async () => {
    const deps = makeDeps();
    const strict = { ...env, POW_BITS: "8" };
    const body = JSON.stringify(report());
    expect((await post("/report", body, deps, {}, strict)).status).toBe(403);
    const good = await stamp(body, 8, deps.at / 1000);
    expect((await post("/report", body, deps, { headers: { "x-rawtonic-pow": good } }, strict)).status).toBe(201);
    expect((await post("/report", body, deps, { headers: { "x-rawtonic-pow": good } }, strict)).status).toBe(403);
    const old = await stamp(body, 8, deps.at / 1000 - 3600);
    expect((await post("/report", body, deps, { headers: { "x-rawtonic-pow": old } }, strict)).status).toBe(403);
  });
});

describe("photo files", () => {
  const bytes = new TextEncoder().encode("not really a RAW file, but bytes");
  const withPhoto = async (extra: Record<string, unknown> = {}, sha?: string) =>
    report({
      photo: { make: "FUJIFILM", model: "X-T5", fileType: "RAF", file: { size: bytes.byteLength, sha256: sha ?? (await sha256Hex(bytes)) } },
      ...extra,
    });
  const announce = async (deps: Deps, extra: Record<string, unknown> = {}, sha?: string) =>
    (await (await post("/report", await withPhoto(extra, sha), deps)).json()) as Answer;
  const upload = (url: string, deps: Deps, body: Uint8Array = bytes) => put(url, deps, body, body.byteLength);

  it("takes the photo's bytes once beside the report, and deletes them with it", async () => {
    const deps = makeDeps();
    const answer = await announce(deps);
    expect(answer.uploads.screenshot).toBeUndefined();
    expect((await upload(answer.uploads.photo!, deps)).status).toBe(204);
    expect((await upload(answer.uploads.photo!, deps)).status).toBe(404);
    const { objects } = await env.BUCKET.list();
    expect(objects.map((o) => o.key)).toEqual([`reports/${await sha256Hex(answer.id)}/photo.RAF`]);
    const [issue] = deps.tracker.issues.values();
    expect(issue.body).toContain(objects[0].key);
    expect(await env.DB.prepare("SELECT state, objects FROM submissions").first()).toEqual({
      state: "filed",
      objects: JSON.stringify([objects[0].key]),
    });
    expect((await del(answer.id, deps)).status).toBe(204);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    expect(deps.tracker.issues.size).toBe(0);
  });

  it("takes a screenshot and the photo of one report, one after the other", async () => {
    const deps = makeDeps();
    const answer = await announce(deps, { screenshot: await shot() });
    // While one file is written, the other waits (and keeps its token).
    await env.DB.prepare("UPDATE submissions SET state = 'attaching'").run();
    expect((await upload(answer.uploads.photo!, deps)).status).toBe(409);
    await env.DB.prepare("UPDATE submissions SET state = 'filed'").run();
    expect((await put(answer.uploads.screenshot!, deps, PNG, PNG.byteLength)).status).toBe(204);
    expect((await upload(answer.uploads.photo!, deps)).status).toBe(204);
    const keys = (await env.BUCKET.list()).objects.map((o) => o.key).sort();
    expect(keys.map((k) => k.replace(/^reports\/[0-9a-f]{64}\//, ""))).toEqual(["photo.RAF", "screenshot.png"]);
    const row = await env.DB.prepare("SELECT objects FROM submissions").first<{ objects: string }>();
    expect((JSON.parse(row!.objects) as string[]).sort()).toEqual(keys);
    expect((await del(answer.id, deps)).status).toBe(204);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
  });

  it("refuses bytes whose SHA-256 differs, and a wrong size", async () => {
    const deps = makeDeps();
    const a = await announce(deps, {}, "0".repeat(64));
    expect((await upload(a.uploads.photo!, deps)).status).toBe(422);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    const b = await announce(deps);
    expect((await upload(b.uploads.photo!, deps, bytes.slice(1))).status).toBe(400);
    expect(await env.DB.prepare("SELECT DISTINCT state, objects FROM submissions").all().then((r) => r.results)).toEqual([
      { state: "filed", objects: "[]" },
    ]);
  });

  it("keeps the report without the file when recording the file fails", async () => {
    const deps = makeDeps();
    const answer = await announce(deps);
    const failed = await handle(
      new Request(answer.uploads.photo!, { method: "PUT", headers: { "content-length": String(bytes.byteLength) }, body: bytes }),
      failingDb(/SET state = 'filed' WHERE/),
      deps,
    );
    expect(failed.status).toBe(502);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    expect(await env.DB.prepare("SELECT state, objects FROM submissions").first()).toEqual({ state: "filed", objects: "[]" });
    expect((await del(answer.id, deps)).status).toBe(204);
  });

  it("expires upload slots after an hour", async () => {
    const deps = makeDeps();
    const a = await announce(deps);
    deps.at += 3_600_001;
    expect((await upload(a.uploads.photo!, deps)).status).toBe(404);
    await sweep(env, deps);
    expect(await count("uploads")).toBe(0);
    expect(await count("submissions")).toBe(1);
  });

  it("has no RAW sample route any more", async () => {
    const deps = makeDeps();
    expect((await post("/raw-sample", { schema: 1 }, deps)).status).toBe(404);
  });
});

describe("retention", () => {
  it("deletes reports after 90 days and expired limit rows", async () => {
    const deps = makeDeps();
    await post("/report", report(), deps);
    deps.at += 89 * DAY_MS;
    await post("/report", report(), deps);
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(2);
    deps.at += 2 * DAY_MS;
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(1);
    const rate = await env.DB.prepare("SELECT COUNT(*) AS n FROM rate WHERE expires < ?1").bind(deps.at).first<{ n: number }>();
    expect(rate!.n).toBe(0);
  });
});

describe("routing", () => {
  it("answers 404 for anything else and sends no CORS headers", async () => {
    const response = await handle(new Request("https://relay.test/", { method: "GET" }), env, makeDeps());
    expect(response.status).toBe(404);
    expect(response.headers.get("access-control-allow-origin")).toBeNull();
  });
});
