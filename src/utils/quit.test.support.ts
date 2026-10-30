// The real queue and Tauri listener bridge, with only the native transport
// replaced. Each simulated command stays pending until its test answers it.
import { act, renderHook } from '@testing-library/react';
import { vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useTauriListeners } from '../hooks/useTauriListeners';
import { photoBatches, submitPhotoWrite } from './photoWrites';
import { Invokes } from '../components/ui/AppProperties';

export function COMMANDS(): string[] {
  return vi.mocked(invoke).mock.calls.map(([name]) => name).filter((name) => name !== Invokes.ExportQueueJobs);
}

export async function mountAppListeners(): Promise<() => void> {
  const props = { refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(), refreshImageList: vi.fn(), markGenerated: vi.fn() };
  const view = renderHook(() => useTauriListeners(props));
  await vi.waitFor(() => {
    if (!vi.mocked(listen).mock.calls.some(([name]) => name === 'batch-write-settled')) throw new Error('bridge not mounted');
  });
  return view.unmount;
}

export class BatchCommand {
  private resolve!: (failed: string[]) => void;
  private reject!: (error: unknown) => void;
  private readonly command: Promise<string[]>;
  readonly opened: Promise<string[]>;
  readonly started: Promise<void>;
  tag = '';
  answered = false;

  constructor(readonly paths: string[]) {
    this.command = new Promise((resolve, reject) => { this.resolve = resolve; this.reject = reject; });
    let start!: () => void;
    this.started = new Promise((resolve) => { start = resolve; });
    this.opened = submitPhotoWrite(paths, (id) => {
      this.tag = photoBatches().get(id)!.tag;
      start();
      return this.command;
    }, { kind: 'auto', batch: true, failedOf: (failed) => failed });
    void this.opened.catch(() => undefined);
  }

  photo(path: string, why: string | null = null): void {
    const callback = vi.mocked(listen).mock.calls.find(([name]) => name === 'batch-write-settled')?.[1];
    if (!callback) throw new Error('bridge not mounted');
    act(() => callback({ event: 'batch-write-settled', id: 0, payload: { batch: this.tag, path, ok: why, also: [] } }));
  }

  finish(failed: string[] = []): void {
    this.answered = true;
    this.resolve(failed);
  }

  fail(): void {
    this.answered = true;
    this.reject({ message: 'Auto adjustments failed', failed: this.paths });
  }
}
