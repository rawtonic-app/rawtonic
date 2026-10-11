import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { createThumbnailQueue, useThumbnails } from './useThumbnails';
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(undefined) }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

describe('visible thumbnail queue', () => {
  it('sends the first viewport immediately and coalesces in-flight scroll requests to the latest one', async () => {
    const pending: (() => void)[] = [];
    const send = vi.fn(() => new Promise<void>((resolve) => pending.push(resolve)));
    const queue = createThumbnailQueue(send);
    queue.request(['first']);
    expect(send).toHaveBeenCalledWith(['first']);
    queue.request(['obsolete']);
    queue.request(['latest']);
    expect(send).toHaveBeenCalledTimes(1);
    pending.shift()!();
    await Promise.resolve();
    await Promise.resolve();
    expect(send.mock.calls).toEqual([[['first']], [['latest']]]);
    pending.shift()!();
  });

  it('clears obsolete pending work and permits regeneration after a folder refresh', async () => {
    let finish!: () => void;
    const send = vi.fn<(paths: string[]) => Promise<void>>()
      .mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }))
      .mockResolvedValue(undefined);
    const queue = createThumbnailQueue(send);
    queue.request(['old']);
    queue.markGenerated('new');
    queue.request(['new']);
    queue.clear();
    finish();
    await Promise.resolve();
    await Promise.resolve();
    expect(send.mock.calls).toEqual([[['old']], [[]]]);
    await Promise.resolve();
    await Promise.resolve();
    queue.request(['new']);
    expect(send.mock.calls.at(-1)).toEqual([['new']]);
  });

  it('does not resubmit completed thumbnails or unchanged ranges', async () => {
    const send = vi.fn().mockResolvedValue(undefined);
    const queue = createThumbnailQueue(send);
    queue.markGenerated('ready');
    queue.request(['ready', 'missing']);
    queue.request(['ready', 'missing']);
    expect(send.mock.calls).toEqual([[['missing']]]);
    await Promise.resolve();
  });

  it('connects the hook to native queue replacement, including clearing an empty viewport', async () => {
    const { result } = renderHook(useThumbnails);
    await act(async () => {
      result.current.requestThumbnails(['offscreen']);
      result.current.requestThumbnails(['visible', 'ready']);
      result.current.markGenerated('ready');
    });
    expect(invoke).toHaveBeenLastCalledWith('update_thumbnail_queue', { paths: ['visible'] });
    const calls = vi.mocked(invoke).mock.calls.length;
    await act(async () => result.current.requestThumbnails(['visible', 'ready']));
    expect(invoke).toHaveBeenCalledTimes(calls);
    await act(async () => result.current.requestThumbnails([]));
    expect(invoke).toHaveBeenLastCalledWith('update_thumbnail_queue', { paths: [] });
  });
});
