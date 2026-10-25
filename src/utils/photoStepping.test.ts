import { describe, expect, it } from "vitest";
import { autoAdvanceTarget, neighboursToPrepare, sharedFlagWrite, stepDirection } from "./photoStepping";

const paths = ["a", "b", "c", "d", "e"];

describe("stepDirection", () => {
  it("tells steps forward and back, also where the arrows wrap", () => {
    expect(stepDirection(1, 2, 5)).toBe(1);
    expect(stepDirection(4, 0, 5)).toBe(1);
    expect(stepDirection(2, 1, 5)).toBe(-1);
    expect(stepDirection(0, 4, 5)).toBe(-1);
  });

  it("calls anything else a jump", () => {
    expect(stepDirection(0, 3, 5)).toBeNull();
    expect(stepDirection(-1, 2, 5)).toBeNull();
    expect(stepDirection(2, 2, 5)).toBeNull();
  });
});

describe("neighboursToPrepare", () => {
  it("prepares two ahead in the step's direction before the one behind", () => {
    expect(neighboursToPrepare(paths, 2, 1)).toEqual(["d", "e", "b"]);
    expect(neighboursToPrepare(paths, 2, -1)).toEqual(["b", "a", "d"]);
  });

  it("goes forward after a jump and leaves out what is past the ends", () => {
    expect(neighboursToPrepare(paths, 3, null)).toEqual(["e", "c"]);
    expect(neighboursToPrepare(paths, 0, -1)).toEqual(["b"]);
    expect(neighboursToPrepare(paths, -1, 1)).toEqual([]);
  });
});

describe("autoAdvanceTarget", () => {
  it("steps to the next photograph after the rated one", () => {
    expect(autoAdvanceTarget(paths, "b", "b")).toBe("c");
  });

  it("does not wrap at the end, nor step again once another photograph is open", () => {
    expect(autoAdvanceTarget(paths, "e", "e")).toBeNull();
    expect(autoAdvanceTarget(paths, "c", "b")).toBeNull();
    expect(autoAdvanceTarget(paths, "x", "x")).toBeNull();
  });
});

describe("sharedFlagWrite", () => {
  it("tells the rated photograph's own flag, acknowledged on a virtual copy of it, from another flag", () => {
    expect(sharedFlagWrite("/s/a.NEF", "/s/a.NEF?vc=2", -1, -1)).toBe(true);
    expect(sharedFlagWrite("/s/a.NEF", "/s/a.NEF?vc=2", -1, 1)).toBe(false);
    expect(sharedFlagWrite("/s/a.NEF", "/s/b.NEF", -1, -1)).toBe(false);
    expect(sharedFlagWrite("/s/a.NEF", "/s/a.NEF?vc=2", undefined, 0)).toBe(false);
  });
});
