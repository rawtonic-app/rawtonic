// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { DAY_MS, ID_PATTERN, fence, isoWeek, newId, weekEnd } from "../src/util";

describe("util", () => {
  it("makes 26-character base32 ids that differ", () => {
    const ids = new Set(Array.from({ length: 200 }, newId));
    expect(ids.size).toBe(200);
    for (const id of ids) expect(id).toMatch(ID_PATTERN);
  });

  it("names ISO weeks across year ends", () => {
    expect(isoWeek(Date.UTC(2026, 9, 24))).toBe("2026-W43");
    expect(isoWeek(Date.UTC(2021, 0, 3))).toBe("2020-W53");
    expect(isoWeek(Date.UTC(2024, 11, 30))).toBe("2025-W01");
  });

  it("ends a week on the next Monday and refuses weeks that do not exist", () => {
    expect(weekEnd("2026-W43")).toBe(Date.UTC(2026, 9, 26));
    expect(weekEnd("2020-W53")).toBe(Date.UTC(2021, 0, 4));
    expect(weekEnd("2025-W53")).toBeNull();
    expect(weekEnd("2026-W00")).toBeNull();
    for (let day = 0; day < 800; day++) {
      const ms = Date.UTC(2025, 0, 1) + day * DAY_MS;
      const end = weekEnd(isoWeek(ms))!;
      expect(end > ms && end - ms <= 7 * DAY_MS).toBe(true);
    }
  });

  it("fences text so it cannot close the block", () => {
    expect(fence("a ``` b")).toBe("````text\na ``` b\n````");
    expect(fence("plain")).toBe("```text\nplain\n```");
  });
});
