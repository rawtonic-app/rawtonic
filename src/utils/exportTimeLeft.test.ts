import { expect, it } from 'vitest';
import type { ExportItem } from '../store/useProcessStore';
import { exportTimeLeft, splitTimeLeft } from './exportTimeLeft';

const item = (index: number, part: Partial<ExportItem>): ExportItem => ({
  index,
  path: `/p/${index}.RAF`,
  state: 'pending',
  output: null,
  error: null,
  attempts: 0,
  startedMs: null,
  finishedMs: null,
  ...part,
});

it('waits for a finished photo before estimating', () => {
  expect(exportTimeLeft([item(0, { state: 'running', startedMs: 0 }), item(1, {})], 5000)).toBeNull();
});

it('spreads the photos not started over the workers in use', () => {
  // Photos take 10 s; two run (4 s and 9 s in), four wait.
  const items = [
    item(0, { state: 'done', startedMs: 0, finishedMs: 10_000 }),
    item(1, { state: 'done', startedMs: 0, finishedMs: 10_000 }),
    item(2, { state: 'running', startedMs: 16_000 }),
    item(3, { state: 'running', startedMs: 11_000 }),
    ...[4, 5, 6, 7].map((i) => item(i, {})),
  ];
  // (6 s + 1 s + 4 × 10 s) / 2 workers.
  expect(exportTimeLeft(items, 20_000)).toBe(23_500);
});

it('ends with the slowest photo in progress and ignores failures', () => {
  const items = [
    item(0, { state: 'done', startedMs: 0, finishedMs: 8_000 }),
    item(1, { state: 'failed', startedMs: 0, finishedMs: 100, error: 'x' }),
    item(2, { state: 'running', startedMs: 5_000 }),
    item(3, { state: 'running', startedMs: 9_000 }),
  ];
  expect(exportTimeLeft(items, 10_000)).toBe(7_000);
  // Overdue photos count as nearly done, never negative.
  expect(exportTimeLeft(items, 30_000)).toBe(0);
  expect(exportTimeLeft(items.slice(0, 2), 30_000)).toBe(0);
});

it('rounds what is left up for reading', () => {
  expect(splitTimeLeft(0)).toEqual({ hours: 0, minutes: 0, seconds: 5 });
  expect(splitTimeLeft(41_000)).toEqual({ hours: 0, minutes: 0, seconds: 45 });
  expect(splitTimeLeft(61_000)).toEqual({ hours: 0, minutes: 2, seconds: 0 });
  expect(splitTimeLeft(3_700_000)).toEqual({ hours: 1, minutes: 2, seconds: 0 });
});
