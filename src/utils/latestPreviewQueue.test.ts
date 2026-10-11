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
