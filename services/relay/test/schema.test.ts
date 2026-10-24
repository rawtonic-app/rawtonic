// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { Invalid, hasHomePath, parseRawSample, parseReport, parseStats } from "../src/schema";
import { APP } from "./helpers";

const field = (f: () => unknown) => {
  try {
    f();
  } catch (e) {
    if (e instanceof Invalid) return e.field;
    throw e;
  }
  return null;
};

describe("schemas", () => {
  it("accepts a minimal report and refuses unknown fields", () => {
    expect(parseReport({ schema: 1, app: APP, text: "Export hangs" }).text).toBe("Export hangs");
    expect(field(() => parseReport({ schema: 1, app: APP, text: "x", email: "a@b.c" }))).toBe("body.email");
    expect(field(() => parseReport({ schema: 1, app: { ...APP, user: "anna" }, text: "x" }))).toBe("app.user");
    expect(field(() => parseReport({ schema: 2, app: APP, text: "x" }))).toBe("schema");
  });

  it("caps text and refuses control characters", () => {
    expect(field(() => parseReport({ schema: 1, app: APP, text: "a".repeat(10_001) }))).toBe("text");
    expect(field(() => parseReport({ schema: 1, app: APP, text: "bell\u0007" }))).toBe("text");
    expect(field(() => parseReport({ schema: 1, app: APP, text: "   " }))).toBe("text");
    expect(parseReport({ schema: 1, app: APP, text: "line\r\nline\ttab" }).text).toContain("\t");
  });

  it("allows only plain names for cameras", () => {
    const photo = (model: string) => ({ schema: 1, app: APP, text: "x", photo: { make: "Sony", model, fileType: "ARW" } });
    expect(parseReport(photo("ILCE-7M3")).photo?.model).toBe("ILCE-7M3");
    expect(field(() => parseReport(photo("@maintainer")))).toBe("photo.model");
    expect(field(() => parseReport(photo("a\nb")))).toBe("photo.model");
  });

  it("needs the sample licence and the personal-data acknowledgement", () => {
    const sample = {
      schema: 1,
      app: APP,
      camera: { make: "Canon", model: "EOS R5" },
      fileType: "CR3",
      size: 1000,
      sha256: "a".repeat(64),
      licence: "rawtonic-sample-1",
      personalDataAcknowledged: true,
    };
    expect(parseRawSample(sample).size).toBe(1000);
    expect(field(() => parseRawSample({ ...sample, personalDataAcknowledged: false }))).toBe("personalDataAcknowledged");
    expect(field(() => parseRawSample({ ...sample, licence: "cc0" }))).toBe("licence");
    expect(field(() => parseRawSample({ ...sample, size: 100 * 1024 * 1024 + 1 }))).toBe("size");
  });

  it("takes only listed tools, as true", () => {
    const stats = { schema: 1, week: "2026-W42", app: APP, gpuVendor: "apple", cameras: [], tools: { masks: true } };
    expect(parseStats(stats).tools).toEqual({ masks: true });
    expect(field(() => parseStats({ ...stats, tools: { masks: 3 } }))).toBe("tools.masks");
    expect(field(() => parseStats({ ...stats, tools: { secretTool: true } }))).toBe("tools.secretTool");
  });

  it("finds home-folder paths anywhere in a body", () => {
    expect(hasHomePath({ a: ["see /Users/anna/Pictures/x.CR3"] })).toBe(true);
    expect(hasHomePath("C:\\Users\\anna\\x.NEF")).toBe(true);
    expect(hasHomePath("/home/anna/x")).toBe(true);
    expect(hasHomePath("<path>.CR3 under /Users/<user>")).toBe(false);
  });
});
