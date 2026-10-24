// SPDX-License-Identifier: AGPL-3.0-or-later
// Weekly usage statistics as counters only (ADR 0017): a send increments one
// counter per (week, dimension, value) and is not stored itself.

import type { Stats } from "./schema";
import { DAY_MS, weekEnd } from "./util";

/** Values counted fewer times in a week are shown, and later kept, only as `(other)`. */
export const K = 10;
export const OTHER = "(other)";
/** A week is accepted for this long after it ended; then it is closed. */
export const ACCEPT_DAYS = 14;
export const KEEP_DAYS = 730;

/** Whether a send for `week` is accepted now: ended, and not long ago. */
export const weekOpen = (week: string, now: number): boolean => {
  const end = weekEnd(week);
  return end !== null && end <= now && now - end <= ACCEPT_DAYS * DAY_MS;
};

/** The counters one send adds to, each value once. */
export const values = (stats: Stats): [string, string][] => {
  const pairs: [string, string][] = [
    ["sends", "all"],
    ["version", stats.app.version],
    ["os", `${stats.app.os} ${stats.app.osVersion}`],
    ["arch", stats.app.arch],
    ["gpu", stats.gpuVendor],
  ];
  const cameras = new Set(stats.cameras.map((c) => `${c.make} ${c.model}`));
  for (const camera of cameras) pairs.push(["camera", camera]);
  for (const tool of Object.keys(stats.tools)) pairs.push(["tool", tool]);
  return pairs;
};

export const record = async (db: D1Database, stats: Stats) => {
  const insert = db.prepare(
    "INSERT INTO stats (week, dimension, value, count) VALUES (?1, ?2, ?3, 1) " +
      "ON CONFLICT (week, dimension, value) DO UPDATE SET count = count + 1",
  );
  await db.batch(values(stats).map(([dimension, value]) => insert.bind(stats.week, dimension, value)));
};

export type Summary = Record<string, { value: string; count: number }[]>;

/** A week's counters with every value below K folded into `(other)`. */
export const summary = async (db: D1Database, week: string): Promise<Summary> => {
  const { results } = await db
    .prepare("SELECT dimension, value, count FROM stats WHERE week = ?1 ORDER BY dimension, count DESC, value")
    .bind(week)
    .all<{ dimension: string; value: string; count: number }>();
  const out: Summary = {};
  const other: Record<string, number> = {};
  for (const row of results) {
    out[row.dimension] ??= [];
    if (row.value !== OTHER && row.count >= K) out[row.dimension].push({ value: row.value, count: row.count });
    else other[row.dimension] = (other[row.dimension] ?? 0) + row.count;
  }
  for (const [dimension, count] of Object.entries(other)) out[dimension].push({ value: OTHER, count });
  return out;
};

/** Closes the weeks no longer accepted: their values below K become one
 * `(other)` counter per dimension and are deleted; drops weeks past KEEP_DAYS. */
export const closeWeeks = async (db: D1Database, now: number) => {
  const { results } = await db
    .prepare("SELECT DISTINCT week FROM stats WHERE week NOT IN (SELECT week FROM stats_closed)")
    .all<{ week: string }>();
  for (const { week } of results) {
    const end = weekEnd(week);
    if (end === null || now - end <= (ACCEPT_DAYS + 1) * DAY_MS) continue;
    await db.batch([
      db
        .prepare(
          "INSERT INTO stats (week, dimension, value, count) " +
            "SELECT week, dimension, ?2, SUM(count) FROM stats " +
            "WHERE week = ?1 AND value != ?2 AND count < ?3 GROUP BY dimension " +
            "ON CONFLICT (week, dimension, value) DO UPDATE SET count = count + excluded.count",
        )
        .bind(week, OTHER, K),
      db.prepare("DELETE FROM stats WHERE week = ?1 AND value != ?2 AND count < ?3").bind(week, OTHER, K),
      db.prepare("INSERT INTO stats_closed (week) VALUES (?1)").bind(week),
    ]);
  }
  const { results: closed } = await db.prepare("SELECT week FROM stats_closed").all<{ week: string }>();
  for (const { week } of closed) {
    const end = weekEnd(week);
    if (end !== null && now - end > KEEP_DAYS * DAY_MS) {
      await db.batch([
        db.prepare("DELETE FROM stats WHERE week = ?1").bind(week),
        db.prepare("DELETE FROM stats_closed WHERE week = ?1").bind(week),
      ]);
    }
  }
};
