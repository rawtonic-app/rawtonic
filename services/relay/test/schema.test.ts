// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { Invalid, hasHomePath, parseReport, parseStats } from "../src/schema";
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
    expect(parseReport({ schema: 2, app: APP, text: "Export hangs" }).text).toBe("Export hangs");
    expect(field(() => parseReport({ schema: 2, app: APP, text: "x", email: "a@b.c" }))).toBe("body.email");
    expect(field(() => parseReport({ schema: 2, app: { ...APP, user: "anna" }, text: "x" }))).toBe("app.user");
    expect(field(() => parseReport({ schema: 1, app: APP, text: "x" }))).toBe("schema");
  });

  it("takes a screenshot only as an announcement of its size and SHA-256", () => {
    const shot = (screenshot: unknown) => ({ schema: 2, app: APP, text: "x", screenshot });
    const ok = { mime: "image/png", size: 2048, sha256: "b".repeat(64) };
    expect(parseReport(shot(ok)).screenshot).toEqual(ok);
    expect(field(() => parseReport(shot({ mime: "image/png", data: "iVBORw0KGgo=" })))).toBe("screenshot.data");
    expect(field(() => parseReport(shot({ ...ok, size: 2 * 1024 * 1024 + 1 })))).toBe("screenshot.size");
    expect(field(() => parseReport(shot({ ...ok, size: 0 })))).toBe("screenshot.size");
    expect(field(() => parseReport(shot({ ...ok, sha256: "B".repeat(64) })))).toBe("screenshot.sha256");
    expect(field(() => parseReport(shot({ ...ok, mime: "image/gif" })))).toBe("screenshot.mime");
  });

  it("caps text and refuses control characters", () => {
    expect(field(() => parseReport({ schema: 2, app: APP, text: "a".repeat(10_001) }))).toBe("text");
    expect(field(() => parseReport({ schema: 2, app: APP, text: "bell\u0007" }))).toBe("text");
    expect(field(() => parseReport({ schema: 2, app: APP, text: "   " }))).toBe("text");
    expect(parseReport({ schema: 2, app: APP, text: "line\r\nline\ttab" }).text).toContain("\t");
  });

  it("allows only plain names for cameras", () => {
    const photo = (model: string) => ({ schema: 2, app: APP, text: "x", photo: { make: "Sony", model, fileType: "ARW" } });
    expect(parseReport(photo("ILCE-7M3")).photo?.model).toBe("ILCE-7M3");
    expect(field(() => parseReport(photo("@maintainer")))).toBe("photo.model");
    expect(field(() => parseReport(photo("a\nb")))).toBe("photo.model");
  });

  it("takes the photo's file only as an announcement, up to 100 MiB", () => {
    const photo = (file: unknown) => ({ schema: 2, app: APP, text: "x", photo: { make: "Canon", model: "EOS R5", fileType: "CR3", file } });
    const ok = { size: 30_000_000, sha256: "c".repeat(64) };
    expect(parseReport(photo(ok)).photo?.file).toEqual(ok);
    expect(field(() => parseReport(photo({ ...ok, size: 100 * 1024 * 1024 + 1 })))).toBe("photo.file.size");
    expect(field(() => parseReport(photo({ ...ok, path: "x" })))).toBe("photo.file.path");
    expect(field(() => parseReport(photo("x")))).toBe("photo.file");
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
