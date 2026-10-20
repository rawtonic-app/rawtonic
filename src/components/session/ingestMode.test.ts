import { describe, expect, it } from "vitest";
import { hasRawJpegPairs, preselectIngestMode } from "./ingestMode";

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

  it("a catalog defaults to portable copies even for photographs on drives", () => {
    expect(preselectIngestMode({ catalog: true, files: [disk, disk], intoFolder: false })).toEqual({
      mode: "copy",
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

const RAW = ["arw", "cr3", "dng", "nef", "raf"];

describe("hasRawJpegPairs", () => {
  it("finds a RAW and a JPEG or HEIF of one name in one folder, letter case aside", () => {
    expect(hasRawJpegPairs(["/c/100/A.ARW", "/c/100/a.jpg"], RAW)).toBe(true);
    expect(hasRawJpegPairs(["C:\\DCIM\\B.CR3", "C:\\DCIM\\B.HIF"], RAW)).toBe(true);
  });
  it("leaves out other folders, three files of a name and developed pairs", () => {
    expect(hasRawJpegPairs(["/c/100/A.ARW", "/c/101/A.JPG"], RAW)).toBe(false);
    expect(hasRawJpegPairs(["/c/A.CR3", "/c/A.DNG", "/c/A.JPG"], RAW)).toBe(false);
    expect(hasRawJpegPairs(["/c/A.TIF", "/c/A.JPG"], RAW)).toBe(false);
    expect(hasRawJpegPairs(["/c/A.ARW", "/c/A.PNG"], RAW)).toBe(false);
    // Only the app's RAW formats are RAWs (not GIF, EXR, …).
    expect(hasRawJpegPairs(["/c/A.GIF", "/c/A.JPG"], RAW)).toBe(false);
    expect(hasRawJpegPairs(["/c/A.EXR", "/c/A.JPG"], RAW)).toBe(false);
  });
});
