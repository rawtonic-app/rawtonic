// SPDX-License-Identifier: AGPL-3.0-or-later
// Rate limits per IP hash, global caps and the optional proof of work
// (ADR 0017, "Abuse protection").

import { DAY_MS, HOUR_MS, hmacHex, isoWeek, sha256Hex, utcDay, weekEnd } from "./util";

export type Kind = "report" | "crash" | "camera-test" | "stats" | "delete";

interface Window {
  /** The window's id (it is mixed into the IP hash) and its end. */
  of(now: number): { id: string; end: number };
  limit: number;
}

const hourly = (limit: number): Window => ({
  limit,
  of: (now) => {
    const start = Math.floor(now / HOUR_MS) * HOUR_MS;
    return { id: `h${start / HOUR_MS}`, end: start + HOUR_MS };
  },
});
const daily = (limit: number): Window => ({
  limit,
  of: (now) => {
    const start = Math.floor(now / DAY_MS) * DAY_MS;
    return { id: utcDay(now), end: start + DAY_MS };
  },
});
const weekly = (limit: number): Window => ({
  limit,
  of: (now) => {
    const id = isoWeek(now);
    return { id, end: weekEnd(id)! };
  },
});

/** Per IP hash. */
export const LIMITS: Record<Kind, Window[]> = {
  report: [hourly(5), daily(20)],
  crash: [daily(10)],
  "camera-test": [daily(10)],
  stats: [weekly(2)],
  delete: [hourly(30)],
};

/** For everyone together, per UTC day. */
export const GLOBAL_CAPS = { issues: 400, uploadBytes: 5 * 1024 * 1024 * 1024 };

/** Adds `amount` to a counter and returns the new value. Rows live until
 * their window ends (the scheduled run deletes them). */
const bump = async (db: D1Database, key: string, amount: number, expires: number): Promise<number> => {
  const row = await db
    .prepare(
      "INSERT INTO rate (key, count, expires) VALUES (?1, ?2, ?3) " +
        "ON CONFLICT (key) DO UPDATE SET count = count + ?2 RETURNING count",
    )
    .bind(key, amount, expires)
    .first<{ count: number }>();
  return row!.count;
};

/** Counts one request of `kind` from `ip`; false when over a limit. The IP
 * is only stored as HMAC(key, window id | ip), so it changes every window. */
export const allow = async (db: D1Database, hashKey: string, kind: Kind, ip: string, now: number) => {
  let ok = true;
  for (const window of LIMITS[kind]) {
    const { id, end } = window.of(now);
    const hash = await hmacHex(hashKey, `${id}|${ip}`);
    if ((await bump(db, `${kind}:${id}:${hash}`, 1, end)) > window.limit) ok = false;
  }
  return ok;
};

/** Takes `amount` from a global daily cap; false when it would exceed it. */
export const takeGlobal = async (
  db: D1Database,
  cap: keyof typeof GLOBAL_CAPS,
  amount: number,
  now: number,
): Promise<boolean> => {
  const window = daily(GLOBAL_CAPS[cap]).of(now);
  return (await bump(db, `global:${cap}:${window.id}`, amount, window.end)) <= GLOBAL_CAPS[cap];
};

const POW_MAX_AGE_S = 600;

const leadingZeroBits = (hexDigest: string): number => {
  let bits = 0;
  for (const char of hexDigest) {
    const nibble = parseInt(char, 16);
    if (nibble === 0) {
      bits += 4;
      continue;
    }
    return bits + Math.clz32(nibble) - 28;
  }
  return bits;
};

/** The proof-of-work hash of a stamp for a body (`X-Rawtonic-Pow`). */
export const powHash = async (seconds: string, bodyHash: string, nonce: string) =>
  sha256Hex(`rawtonic-pow-1:${seconds}:${bodyHash}:${nonce}`);

/** Checks `X-Rawtonic-Pow` when `bits` > 0: fresh, unused and with enough
 * leading zero bits over this body. */
export const checkPow = async (
  db: D1Database,
  bits: number,
  header: string | null,
  body: Uint8Array,
  now: number,
): Promise<boolean> => {
  if (bits <= 0) return true;
  const match = header && /^(\d{1,12})\.([0-9a-zA-Z]{1,32})$/.exec(header);
  if (!match) return false;
  const seconds = Number(match[1]);
  if (Math.abs(now / 1000 - seconds) > POW_MAX_AGE_S) return false;
  const digest = await powHash(match[1], await sha256Hex(body), match[2]);
  if (leadingZeroBits(digest) < bits) return false;
  const inserted = await db
    .prepare("INSERT INTO pow_seen (stamp, expires) VALUES (?1, ?2) ON CONFLICT DO NOTHING")
    .bind(digest, now + 2 * POW_MAX_AGE_S * 1000)
    .run();
  return inserted.meta.changes === 1;
};
