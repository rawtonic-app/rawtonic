// SPDX-License-Identifier: AGPL-3.0-or-later
// Small helpers: ids, hashes, ISO weeks, Markdown fences.

const BASE32 = "abcdefghijklmnopqrstuvwxyz234567";

/** A submission id: 128 random bits as 26 base32 characters (ADR 0017). */
export const newId = (): string => {
  const bytes = crypto.getRandomValues(new Uint8Array(17));
  let bits = 0;
  let value = 0;
  let out = "";
  for (const byte of bytes) {
    value = (value << 8) | byte;
    bits += 8;
    while (bits >= 5 && out.length < 26) {
      out += BASE32[(value >>> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  return out;
};

export const ID_PATTERN = /^[a-z2-7]{26}$/;

export const hex = (bytes: ArrayBuffer | Uint8Array): string =>
  [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");

export const sha256Hex = async (data: string | Uint8Array): Promise<string> =>
  hex(
    await crypto.subtle.digest(
      "SHA-256",
      typeof data === "string" ? new TextEncoder().encode(data) : data,
    ),
  );

export const hmacHex = async (key: string, message: string): Promise<string> => {
  const cryptoKey = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(key),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  return hex(await crypto.subtle.sign("HMAC", cryptoKey, new TextEncoder().encode(message)));
};

/** Constant-time comparison of two strings (admin token). */
export const sameSecret = (a: string, b: string): boolean => {
  const x = new TextEncoder().encode(a);
  const y = new TextEncoder().encode(b);
  let diff = x.length ^ y.length;
  for (let i = 0; i < Math.max(x.length, y.length); i++) diff |= (x[i] ?? 0) ^ (y[i] ?? 0);
  return diff === 0;
};

export const DAY_MS = 86_400_000;
export const HOUR_MS = 3_600_000;

export const utcDay = (ms: number): string => new Date(ms).toISOString().slice(0, 10);

/** The ISO week ("2026-W43") a moment falls in. */
export const isoWeek = (ms: number): string => {
  const date = new Date(ms);
  const day = (date.getUTCDay() + 6) % 7; // Monday = 0
  const thursday = Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate() - day + 3);
  const year = new Date(thursday).getUTCFullYear();
  const week = Math.floor((thursday - Date.UTC(year, 0, 1)) / (7 * DAY_MS)) + 1;
  return `${year}-W${String(week).padStart(2, "0")}`;
};

export const WEEK_PATTERN = /^(\d{4})-W(\d{2})$/;

/** The end (exclusive, the next Monday 00:00 UTC) of an ISO week, or null. */
export const weekEnd = (week: string): number | null => {
  const match = WEEK_PATTERN.exec(week);
  if (!match) return null;
  const year = Number(match[1]);
  const number = Number(match[2]);
  // Week 1 holds January 4th.
  const jan4 = Date.UTC(year, 0, 4);
  const monday1 = jan4 - ((new Date(jan4).getUTCDay() + 6) % 7) * DAY_MS;
  const start = monday1 + (number - 1) * 7 * DAY_MS;
  if (number < 1 || isoWeek(start) !== week) return null;
  return start + 7 * DAY_MS;
};

/** A fenced Markdown block that the text cannot close early. */
export const fence = (text: string, lang = "text"): string => {
  const longest = Math.max(0, ...[...text.matchAll(/`+/g)].map((m) => m[0].length));
  const ticks = "`".repeat(Math.max(3, longest + 1));
  return `${ticks}${lang}\n${text}\n${ticks}`;
};
