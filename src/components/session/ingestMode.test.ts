import { describe, expect, it } from "vitest";
import { preselectIngestMode } from "./ingestMode";

const card = { medium: "card" as const };
const disk = { medium: "disk" as const };

describe("ingest mode preselection", () => {
  it("a session always copies, without a switch", () => {
    expect(preselectIngestMode({ catalog: false, files: [disk], intoFolder: false })).toEqual({
      mode: "copy",
      showSwitch: false,
      canAdd: false,
    });
  });

  it("a catalog copies photographs off a card and cannot add them where they are", () => {
    expect(preselectIngestMode({ catalog: true, files: [card, card], intoFolder: false })).toEqual({
      mode: "copy",
      showSwitch: true,
      canAdd: false,
    });
  });

  it("a catalog adds photographs on drives where they are", () => {
    expect(preselectIngestMode({ catalog: true, files: [disk, disk], intoFolder: false })).toEqual({
      mode: "add",
      showSwitch: true,
      canAdd: true,
    });
  });

  it("copies into a folder chosen by a drop or “Import into this folder…”, add still possible", () => {
    expect(preselectIngestMode({ catalog: true, files: [disk], intoFolder: true })).toEqual({
      mode: "copy",
      showSwitch: true,
      canAdd: true,
    });
  });

  it("copies mixed card and drive sources, add disabled", () => {
    expect(preselectIngestMode({ catalog: true, files: [disk, card], intoFolder: false })).toMatchObject({
      mode: "copy",
      canAdd: false,
    });
  });

  it("with no sources yet copies", () => {
    expect(preselectIngestMode({ catalog: true, files: [], intoFolder: false }).mode).toBe("copy");
  });
});
