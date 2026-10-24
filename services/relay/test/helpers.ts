// SPDX-License-Identifier: AGPL-3.0-or-later
import { applyD1Migrations, type D1Migration } from "cloudflare:test";
import { env as workerEnv } from "cloudflare:workers";
import type { FiledIssue, IssueTracker, NewIssue } from "../src/github";
import { handle, type Deps, type Env } from "../src/index";

const env = workerEnv as unknown as Env & { TEST_MIGRATIONS: D1Migration[] };
export const testEnv = env;

/** Applies the migrations and empties every table and the bucket. */
export const migrate = async () => {
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
  await env.DB.batch(
    ["submissions", "uploads", "rate", "pow_seen", "stats", "stats_closed"].map((t) => env.DB.prepare(`DELETE FROM ${t}`)),
  );
  const { objects } = await env.BUCKET.list();
  if (objects.length) await env.BUCKET.delete(objects.map((o: R2Object) => o.key));
};

/** An issue tracker in memory. */
export class FakeTracker implements IssueTracker {
  issues = new Map<string, NewIssue & FiledIssue>();
  next = 1;
  failCreate = false;
  failDelete = false;
  async create(issue: NewIssue): Promise<FiledIssue> {
    if (this.failCreate) throw new Error("GitHub down");
    const filed = { number: this.next, nodeId: `I_${this.next}` };
    this.next++;
    this.issues.set(filed.nodeId, { ...issue, ...filed });
    return filed;
  }
  async delete(nodeId: string) {
    if (this.failDelete) throw new Error("GitHub down");
    this.issues.delete(nodeId);
  }
}

export const NOW = Date.UTC(2026, 9, 24, 12); // Saturday of 2026-W43

export const makeDeps = (tracker = new FakeTracker(), now = NOW) => {
  const deps: Deps & { tracker: FakeTracker; at: number } = {
    tracker,
    at: now,
    now: () => deps.at,
  };
  return deps;
};

export const APP = { version: "0.1.0-alpha.4", os: "macos", osVersion: "27.0", arch: "aarch64" };

export const post = (
  path: string,
  body: unknown,
  deps: Deps,
  { ip = "203.0.113.7", headers = {} }: { ip?: string; headers?: Record<string, string> } = {},
  e: Env = env,
) =>
  handle(
    new Request(`https://relay.test${path}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-rawtonic-client": "0.1.0-alpha.4",
        "cf-connecting-ip": ip,
        ...headers,
      },
      body: typeof body === "string" ? body : JSON.stringify(body),
    }),
    e,
    deps,
  );

export const del = (id: string, deps: Deps) =>
  handle(
    new Request(`https://relay.test/report/${id}`, { method: "DELETE", headers: { "cf-connecting-ip": "203.0.113.7" } }),
    env,
    deps,
  );
