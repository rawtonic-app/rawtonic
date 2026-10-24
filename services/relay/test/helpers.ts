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
    ["submissions", "uploads", "rate", "pow_seen", "stats", "stats_closed", "sweep_state"].map((t) => env.DB.prepare(`DELETE FROM ${t}`)),
  );
  const { objects } = await env.BUCKET.list();
  if (objects.length) await env.BUCKET.delete(objects.map((o: R2Object) => o.key));
};

/** An issue tracker in memory. */
export class FakeTracker implements IssueTracker {
  issues = new Map<string, NewIssue & FiledIssue & { createdAt: number }>();
  clock = () => 0;
  next = 1;
  failCreate = false;
  failDelete = false;
  private async createNow(issue: NewIssue): Promise<FiledIssue> {
    if (this.failCreate) throw new Error("GitHub down");
    const filed = { number: this.next, nodeId: `I_${this.next}` };
    this.next++;
    this.issues.set(filed.nodeId, { ...issue, ...filed, createdAt: this.clock() });
    return filed;
  }
  /** create() files the issue but then fails, as a lost answer would. */
  createThenThrow = false;
  failList = false;
  async create(issue: NewIssue): Promise<FiledIssue> {
    const filed = await this.createNow(issue);
    if (this.createThenThrow) throw new Error("answer lost");
    return filed;
  }
  async delete(nodeId: string) {
    if (this.failDelete) throw new Error("GitHub down");
    this.issues.delete(nodeId);
  }
  async listOldest(count: number) {
    if (this.failList) throw new Error("list unavailable");
    return [...this.issues.values()]
      .sort((a, b) => a.createdAt - b.createdAt)
      .slice(0, count)
      .map((i) => ({ nodeId: i.nodeId, createdAt: i.createdAt }));
  }
}

/** The environment with a database whose statements matching `pattern` fail. */
export const failingDb = (pattern: RegExp, e: Env = env): Env => ({
  ...e,
  DB: new Proxy(e.DB, {
    get(target, key) {
      if (key === "prepare") {
        return (sql: string) => {
          if (!pattern.test(sql)) return target.prepare(sql);
          const fail = async () => {
            throw new Error("D1 unavailable");
          };
          const statement = { run: fail, first: fail, all: fail, bind: () => statement, failing: true };
          return statement;
        };
      }
      if (key === "batch") {
        return async (statements: (D1PreparedStatement & { failing?: boolean })[]) => {
          if (statements.some((s) => s.failing)) throw new Error("D1 unavailable");
          return target.batch(statements);
        };
      }
      const value = Reflect.get(target, key);
      return typeof value === "function" ? value.bind(target) : value;
    },
  }),
});

export const NOW = Date.UTC(2026, 9, 24, 12); // Saturday of 2026-W43

export const makeDeps = (tracker = new FakeTracker(), now = NOW) => {
  const deps: Deps & { tracker: FakeTracker; at: number } = {
    tracker,
    at: now,
    now: () => deps.at,
  };
  tracker.clock = () => deps.at;
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
