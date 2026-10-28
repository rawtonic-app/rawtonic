import "../../i18n";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { INITIAL_ADJUSTMENTS } from "../../utils/adjustments";
import { PointCurve } from "./StudioDevelop";

afterEach(cleanup);

describe("point curve keyboard", () => {
  it("adds, moves and removes a point and keeps endpoint constraints", () => {
    const changed = vi.fn();
    function Harness() {
      const [curves, setCurves] = useState(INITIAL_ADJUSTMENTS.curves);
      return <PointCurve curves={curves} onChange={(next) => { changed(next); setCurves(next); }} disabled={false} onInteraction={() => {}} />;
    }
    render(<Harness />);
    const first = screen.getByRole("button", { name: /Point 1, input 0, output 0/i });
    fireEvent.focus(first);
    fireEvent.keyDown(first, { key: "ArrowLeft" });
    expect(changed).not.toHaveBeenCalled();
    fireEvent.keyDown(first, { key: "a" });
    expect(changed).toHaveBeenCalledTimes(1);
    const added = screen.getByRole("button", { name: /Point 2, input 128, output 128/i });
    fireEvent.focus(added);
    fireEvent.keyDown(added, { key: "ArrowUp" });
    expect(changed).toHaveBeenCalledTimes(2);
    const moved = screen.getByRole("button", { name: /Point 2, input 128, output 129/i });
    fireEvent.keyDown(moved, { key: "Delete" });
    expect(changed).toHaveBeenCalledTimes(3);
    expect(screen.queryByRole("button", { name: /Point 2, input 128/i })).toBeNull();
    fireEvent.keyDown(first, { key: "Delete" });
    expect(changed).toHaveBeenCalledTimes(3);
  });
});
