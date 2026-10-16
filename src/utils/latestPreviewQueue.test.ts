import { expect, it, vi } from "vitest";
import { LatestPreviewQueue } from "./latestPreviewQueue";

it("bounds work and dispatches only the newest slider value immediately after completion", async () => {
  const resolves: (() => void)[] = [];
  const render = vi.fn(
    () => new Promise<void>((resolve) => resolves.push(resolve)),
  );
  const queue = new LatestPreviewQueue<number>(render);
  queue.submit(1);
  for (let value = 2; value <= 100; value++) queue.submit(value);
  expect(render.mock.calls).toEqual([[1]]);
  resolves.shift()!();
  await Promise.resolve();
  expect(render.mock.calls).toEqual([[1], [100]]);
  resolves.shift()!();
});

it("replaces a queued drag with the final value and never replays the drag afterward", async () => {
  let finish!: () => void;
  const render = vi.fn(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  const queue = new LatestPreviewQueue<{ value: number; dragging: boolean }>(
    render,
  );
  queue.submit({ value: 1, dragging: true });
  queue.submit({ value: 2, dragging: true });
  queue.submit({ value: 3, dragging: false });
  finish();
  await Promise.resolve();
  expect(render.mock.calls).toEqual([
    [{ value: 1, dragging: true }],
    [{ value: 3, dragging: false }],
  ]);
  finish();
  await Promise.resolve();
  expect(render).toHaveBeenCalledTimes(2);
});

it("discards a previous photograph’s pending work and recovers after a failed frame", async () => {
  let fail!: (error: Error) => void;
  const render = vi.fn(
    () =>
      new Promise<void>((_, reject) => {
        fail = reject;
      }),
  );
  const queue = new LatestPreviewQueue<string>(render);
  queue.submit("old-active");
  queue.submit("old-pending");
  queue.clear();
  queue.submit("new-photo");
  fail(new Error("render failed"));
  await Promise.resolve();
  expect(render.mock.calls).toEqual([["old-active"], ["new-photo"]]);
  queue.clear();
  fail(new Error("render failed"));
  await Promise.resolve();
});

it("counts submitted, superseded and dispatched values", async () => {
  let finish!: () => void;
  const queue = new LatestPreviewQueue<number>(
    () => new Promise<void>((resolve) => { finish = resolve; }),
  );
  for (let value = 1; value <= 5; value++) queue.submit(value);
  expect(queue.stats).toEqual({ submitted: 5, superseded: 3, dispatched: 1 });
  finish();
  await Promise.resolve();
  expect(queue.stats).toEqual({ submitted: 5, superseded: 3, dispatched: 2 });
  finish();
});

it("keeps a second render in flight and still replaces later pending values", async () => {
  const resolves: (() => void)[] = [];
  const render = vi.fn(() => new Promise<void>((resolve) => resolves.push(resolve)));
  const queue = new LatestPreviewQueue<number>(render, 2);
  for (let value = 1; value <= 5; value++) queue.submit(value);
  expect(render.mock.calls).toEqual([[1], [2]]);
  resolves.shift()!();
  await Promise.resolve();
  expect(render.mock.calls).toEqual([[1], [2], [5]]);
  resolves.shift()!();
  resolves.shift()!();
  await Promise.resolve();
  expect(render).toHaveBeenCalledTimes(3);
});

// The queue in front of a model of the native preview worker
// (src-tauri/src/preview_slot.rs: one waiting request, replaced and rejected
// when a newer one arrives) with renders far slower than the slider moves:
// every render starts from the newest move, frames never go back, and the
// release value renders right after the render in progress.
it("with a slow renderer every frame starts from the newest move and the last move renders last", async () => {
  vi.useFakeTimers();
  try {
    const RENDER_MS = 100;
    const MOVE_MS = 16;
    const MOVES = 61;
    type Job = { value: number; done: () => void; superseded: () => void };
    let newestMove = 0;
    let busy = false;
    let waiting: Job | null = null;
    const started: { value: number; newest: number; at: number }[] = [];
    const work = () => {
      if (busy || !waiting) return;
      const job = waiting;
      waiting = null;
      busy = true;
      started.push({ value: job.value, newest: newestMove, at: Date.now() });
      setTimeout(() => {
        busy = false;
        job.done();
        work();
      }, RENDER_MS);
    };
    const queue = new LatestPreviewQueue<number>(
      (value) =>
        new Promise<void>((resolve, reject) => {
          // 1 ms IPC each way.
          setTimeout(() => {
            if (waiting && waiting.value > value) return setTimeout(reject, 1);
            waiting?.superseded();
            waiting = { value, done: () => setTimeout(resolve, 1), superseded: () => setTimeout(reject, 1) };
            work();
          }, 1);
        }),
      4,
    );
    const t0 = Date.now();
    for (let move = 1; move <= MOVES; move++) {
      setTimeout(() => {
        newestMove = move;
        queue.submit(move);
      }, move * MOVE_MS);
    }
    await vi.advanceTimersByTimeAsync(3000);

    const values = started.map((s) => s.value);
    expect(values.at(-1)).toBe(MOVES);
    expect(values.every((v, i) => i === 0 || v > values[i - 1])).toBe(true);
    // At most the move that arrived during the 1 ms IPC hop is newer.
    for (const s of started) expect(s.newest - s.value).toBeLessThanOrEqual(1);
    // The release frame waits only for the render in progress.
    const release = started.find((s) => s.value === MOVES)!;
    expect(release.at - t0 - MOVES * MOVE_MS).toBeLessThanOrEqual(RENDER_MS + 2);
  } finally {
    vi.useRealTimers();
  }
});
