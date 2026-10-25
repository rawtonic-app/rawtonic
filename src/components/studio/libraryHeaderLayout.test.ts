import { describe, expect, it } from "vitest";
import { libraryHeaderFits } from "./libraryHeaderLayout";

describe("Library header breakpoint", () => {
  it.each([1, 1.12, 1.25])("keeps two rows below 1440 logical pixels at scale %s", (scale) => {
    expect(libraryHeaderFits(1280, scale)).toBe(false);
    expect(libraryHeaderFits(1440 * scale - 1, scale)).toBe(false);
    expect(libraryHeaderFits(1440 * scale, scale)).toBe(true);
  });
  it("fits the wide window at 100 and 112 percent, keeps room at 125 percent", () => {
    expect(libraryHeaderFits(1728, 1)).toBe(true);
    expect(libraryHeaderFits(1728, 1.12)).toBe(true);
    expect(libraryHeaderFits(1728, 1.25)).toBe(false);
  });
});
