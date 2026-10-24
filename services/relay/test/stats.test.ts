// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { handle, sweep } from "../src/index";
import { DAY_MS } from "../src/util";
import { APP, makeDeps, migrate, post, testEnv as env } from "./helpers";

const send = (deps: ReturnType<typeof makeDeps>, ip: string, cameras: { make: string; model: string }[], week = "2026-W42") =>
  post("/stats", { schema: 1, week, app: APP, gpuVendor: "apple", cameras, tools: { masks: true } }, deps, { ip });

const summary = async (deps: ReturnType<typeof makeDeps>, week = "2026-W42", token = "test-admin") =>
  handle(
    new Request(`https://relay.test/stats/summary?week=${week}`, { headers: { authorization: `Bearer ${token}` } }),
    env,
    deps,
  );

beforeEach(async () => {
  await migrate();
});

describe("statistics", () => {
  it("keeps counters only and shows values below k as (other)", async () => {
    const deps = makeDeps();
    for (let i = 0; i < 11; i++) {
      const cameras = [{ make: "Sony", model: "ILCE-7M3" }, ...(i === 0 ? [{ make: "Leica", model: "M11" }] : [])];
      expect((await send(deps, `198.51.100.${i}`, cameras)).status).toBe(204);
    }
    const columns = await env.DB.prepare("SELECT * FROM stats LIMIT 1").first();
    expect(Object.keys(columns!).sort()).toEqual(["count", "dimension", "value", "week"]);
    expect((await summary(deps, "2026-W42", "wrong")).status).toBe(401);
    const { counts } = (await (await summary(deps)).json()) as { counts: Record<string, { value: string; count: number }[]> };
    expect(counts.camera).toEqual([
      { value: "Sony ILCE-7M3", count: 11 },
      { value: "(other)", count: 1 },
    ]);
    expect(counts.sends).toEqual([{ value: "all", count: 11 }]);
    expect(counts.tool).toEqual([{ value: "masks", count: 11 }]);
  });

  it("accepts only weeks that ended in the last two weeks", async () => {
    const deps = makeDeps();
    expect((await send(deps, "198.51.100.1", [], "2026-W43")).status).toBe(400); // still running
    expect((await send(deps, "198.51.100.2", [], "2026-W40")).status).toBe(400); // too old
    expect((await send(deps, "198.51.100.3", [], "2026-W41")).status).toBe(204);
  });

  it("allows two sends per IP and week", async () => {
    const deps = makeDeps();
    expect((await send(deps, "198.51.100.1", [])).status).toBe(204);
    expect((await send(deps, "198.51.100.1", [])).status).toBe(204);
    expect((await send(deps, "198.51.100.1", [])).status).toBe(429);
  });

  it("folds rare values away for good once a week is closed", async () => {
    const deps = makeDeps();
    for (let i = 0; i < 10; i++) await send(deps, `198.51.100.${i}`, [{ make: "Sony", model: "ILCE-7M3" }]);
    await send(deps, "198.51.100.99", [{ make: "Leica", model: "M11" }]);
    deps.at += 20 * DAY_MS;
    await sweep(env, deps);
    const rows = await env.DB.prepare("SELECT value, count FROM stats WHERE dimension = 'camera' ORDER BY value").all();
    expect(rows.results).toEqual([
      { value: "(other)", count: 1 },
      { value: "Sony ILCE-7M3", count: 10 },
    ]);
    // Closing twice changes nothing.
    await sweep(env, deps);
    const again = await env.DB.prepare("SELECT value, count FROM stats WHERE dimension = 'camera' ORDER BY value").all();
    expect(again.results).toEqual(rows.results);
  });
});
