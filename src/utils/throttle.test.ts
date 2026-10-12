import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { throttleAsync } from "./throttle";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("throttleAsync", () => {
  it("runs at most four times a second for a burst of calls, and once after the last call", async () => {
    const runs: number[] = [];
    const run = vi.fn(() => void runs.push(Date.now()));
    const refresh = throttleAsync(run, 250);
    const start = Date.now();
    // 40 imported photographs within one second.
    let lastCall = 0;
    for (let i = 0; i < 40; i++) {
      lastCall = Date.now();
      refresh();
      await vi.advanceTimersByTimeAsync(25);
    }
    await vi.advanceTimersByTimeAsync(5000);
    // Runs at least 250 ms apart: four in the burst's second, plus the trailing one.
    expect(runs.length).toBeGreaterThanOrEqual(4);
    expect(runs.length).toBeLessThanOrEqual(5);
    expect(runs.slice(1).every((t, i) => t - runs[i] >= 250)).toBe(true);
    expect(runs.filter((t) => t - start < 1000).length).toBeLessThanOrEqual(4);
    // The last photograph is shown: a run after the last call.
    expect(runs[runs.length - 1]).toBeGreaterThanOrEqual(lastCall);
  });

  it("runs the first call at once", async () => {
    const run = vi.fn();
    throttleAsync(run, 250)();
    await vi.advanceTimersByTimeAsync(0);
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("never runs twice at once: a slow run delays the next", async () => {
    let active = 0;
    let most = 0;
    const run = vi.fn(async () => {
      active++;
      most = Math.max(most, active);
      await new Promise((resolve) => setTimeout(resolve, 600));
      active--;
    });
    const refresh = throttleAsync(run, 100);
    refresh();
    await vi.advanceTimersByTimeAsync(10);
    refresh();
    refresh();
    await vi.advanceTimersByTimeAsync(2000);
    expect(most).toBe(1);
    expect(run).toHaveBeenCalledTimes(2);
  });

  it("cancel drops a waiting run", async () => {
    const run = vi.fn();
    const refresh = throttleAsync(run, 250);
    refresh();
    await vi.advanceTimersByTimeAsync(0);
    refresh();
    refresh.cancel();
    await vi.advanceTimersByTimeAsync(1000);
    expect(run).toHaveBeenCalledTimes(1);
  });
});
