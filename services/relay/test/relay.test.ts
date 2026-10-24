// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { handle, sweep } from "../src/index";
import { powHash } from "../src/limits";
import { DAY_MS, sha256Hex } from "../src/util";
import { APP, del, failingDb, makeDeps, migrate, post, testEnv as env } from "./helpers";

const PNG = Uint8Array.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3]);
const b64 = (bytes: Uint8Array) => btoa(String.fromCharCode(...bytes));

const report = (extra: Record<string, unknown> = {}) => ({ schema: 1, app: APP, text: "Export stops at 40 %", ...extra });

beforeEach(async () => {
  await migrate();
});

describe("reports", () => {
  it("files a report as a private issue, answers an id, and deletes it by that id", async () => {
    const deps = makeDeps();
    const response = await post("/report", report({ screenshot: { mime: "image/png", data: b64(PNG) } }), deps);
    expect(response.status).toBe(201);
    const { id, ref } = (await response.json()) as { id: string; ref: string };
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
    const objects = await env.BUCKET.list({ prefix: "screenshots/" });
    expect(objects.objects).toHaveLength(1);
    expect(issue.body).toContain(objects.objects[0].key);

    expect((await del(id, deps)).status).toBe(204);
    expect(deps.tracker.issues.size).toBe(0);
    expect((await env.BUCKET.list({ prefix: "screenshots/" })).objects).toHaveLength(0);
    expect((await del(id, deps)).status).toBe(404);
  });

  it("refuses bodies that still hold a home-folder path", async () => {
    const deps = makeDeps();
    const response = await post("/report", report({ logTail: "open /Users/anna/Pictures/a.CR3 failed" }), deps);
    expect(response.status).toBe(422);
    expect(deps.tracker.issues.size).toBe(0);
  });

  it("checks content type, client header, size and screenshot bytes", async () => {
    const deps = makeDeps();
    expect((await post("/report", report(), deps, { headers: { "content-type": "text/plain" } })).status).toBe(415);
    expect((await post("/report", report(), deps, { headers: { "x-rawtonic-client": "" } })).status).toBe(400);
    expect((await post("/crash", { ...report(), text: "a".repeat(100 * 1024) }, deps)).status).toBe(413);
    const fake = await post("/report", report({ screenshot: { mime: "image/png", data: b64(new Uint8Array([1, 2, 3, 4])) } }), deps);
    expect(fake.status).toBe(400);
    expect(deps.tracker.issues.size).toBe(0);
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

  it("removes the screenshot again when GitHub fails", async () => {
    const deps = makeDeps();
    deps.tracker.failCreate = true;
    const response = await post("/report", report({ screenshot: { mime: "image/png", data: b64(PNG) } }), deps);
    expect(response.status).toBe(502);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
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
    const response = await post("/report", report({ screenshot: { mime: "image/png", data: b64(PNG) } }), deps, {}, broken);
    expect(response.status).toBe(502);
    expect(deps.tracker.issues.size).toBe(0);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
  });

  it("finds and deletes an unrecorded issue on the next run when cleaning up failed", async () => {
    const deps = makeDeps();
    deps.tracker.failDelete = true;
    const response = await post("/report", report(), deps, {}, failingDb(/SET state = 'filed'/));
    expect(response.status).toBe(502);
    expect(deps.tracker.issues.size).toBe(1);
    deps.tracker.failDelete = false;
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(1); // its request may still be running
    deps.at += 3 * 3_600_000;
    await sweep(env, deps);
    expect(deps.tracker.issues.size).toBe(0);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
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

describe("RAW samples", () => {
  const bytes = new TextEncoder().encode("not really a RAW file, but bytes");
  const announce = async (deps: ReturnType<typeof makeDeps>, sha?: string) =>
    post(
      "/raw-sample",
      {
        schema: 1,
        app: APP,
        camera: { make: "FUJIFILM", model: "X-T5" },
        fileType: "RAF",
        size: bytes.byteLength,
        sha256: sha ?? (await sha256Hex(bytes)),
        licence: "rawtonic-sample-1",
        personalDataAcknowledged: true,
      },
      deps,
    );
  const upload = (url: string, deps: ReturnType<typeof makeDeps>, body: Uint8Array = bytes) =>
    handle(new Request(url, { method: "PUT", headers: { "content-length": String(body.byteLength) }, body }), env, deps);

  it("takes the file once through its upload URL, then files the issue", async () => {
    const deps = makeDeps();
    const answer = (await (await announce(deps)).json()) as { id: string; uploadUrl: string };
    expect(deps.tracker.issues.size).toBe(0);
    expect((await upload(answer.uploadUrl, deps)).status).toBe(204);
    const [issue] = deps.tracker.issues.values();
    expect(issue.labels).toEqual(["raw-sample"]);
    const { objects } = await env.BUCKET.list({ prefix: "raw-samples/" });
    expect(objects).toHaveLength(1);
    expect(objects[0].key).toMatch(/\/FUJIFILM-X-T5\.RAF$/);
    expect((await upload(answer.uploadUrl, deps)).status).toBe(404);
    expect((await del(answer.id, deps)).status).toBe(204);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
  });

  it("refuses bytes whose SHA-256 differs, and a wrong size", async () => {
    const deps = makeDeps();
    const a = (await (await announce(deps, "0".repeat(64))).json()) as { uploadUrl: string };
    expect((await upload(a.uploadUrl, deps)).status).toBe(422);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    const b = (await (await announce(deps)).json()) as { uploadUrl: string };
    expect((await upload(b.uploadUrl, deps, bytes.slice(1))).status).toBe(400);
    expect(deps.tracker.issues.size).toBe(0);
  });

  it("makes a deletion wait while the sample is being filed, then deletes all of it", async () => {
    const deps = makeDeps();
    const answer = (await (await announce(deps)).json()) as { id: string; uploadUrl: string };
    let release!: () => void;
    deps.tracker.gate = new Promise((resolve) => (release = resolve));
    const uploading = upload(answer.uploadUrl, deps);
    while (deps.tracker.creating === 0) await new Promise((r) => setTimeout(r, 1));
    expect((await del(answer.id, deps)).status).toBe(409);
    release();
    expect((await uploading).status).toBe(204);
    expect(deps.tracker.issues.size).toBe(1);
    expect((await del(answer.id, deps)).status).toBe(204);
    expect(deps.tracker.issues.size).toBe(0);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
  });

  it("removes the sample's issue and file when recording them fails", async () => {
    const deps = makeDeps();
    const answer = (await (await announce(deps)).json()) as { uploadUrl: string };
    const response = await handle(
      new Request(answer.uploadUrl, { method: "PUT", headers: { "content-length": String(bytes.byteLength) }, body: bytes }),
      failingDb(/SET state = 'filed'/),
      deps,
    );
    expect(response.status).toBe(502);
    expect(deps.tracker.issues.size).toBe(0);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
  });

  it("keeps a sample without an issue when GitHub fails, still deletable", async () => {
    const deps = makeDeps();
    const answer = (await (await announce(deps)).json()) as { id: string; uploadUrl: string };
    deps.tracker.failCreate = true;
    expect((await upload(answer.uploadUrl, deps)).status).toBe(204);
    expect((await env.BUCKET.list()).objects).toHaveLength(1);
    expect((await del(answer.id, deps)).status).toBe(204);
    expect((await env.BUCKET.list()).objects).toHaveLength(0);
  });

  it("expires announced samples never uploaded", async () => {
    const deps = makeDeps();
    const a = (await (await announce(deps)).json()) as { uploadUrl: string };
    deps.at += 3 * 3_600_000;
    expect((await upload(a.uploadUrl, deps)).status).toBe(404);
    await sweep(env, deps);
    expect((await env.DB.prepare("SELECT COUNT(*) AS n FROM submissions").first<{ n: number }>())!.n).toBe(0);
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
