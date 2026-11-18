import '../../i18n';
import { act } from 'react';
import { afterAll, afterEach, beforeAll, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useEditorStore } from '../../store/useEditorStore';
import type { WaveformData } from '../ui/AppProperties';

// The webview draws scopes in a worker (scopeCanvasWorker.ts); jsdom has no
// OffscreenCanvas, so the support check and the worker are stand-ins.
vi.mock('./scopeDrawer', () => ({ offscreenScopesSupported: () => true }));

class FakeWorker {
  static all: FakeWorker[] = [];
  messages: unknown[] = [];
  terminated = false;
  listeners: Record<string, () => void> = {};
  constructor() {
    FakeWorker.all.push(this);
  }
  postMessage(message: unknown) {
    this.messages.push(message);
  }
  terminate() {
    this.terminated = true;
  }
  addEventListener(type: string, listener: () => void) {
    this.listeners[type] = listener;
  }
}

const { default: StudioScopes } = await import('./StudioScopes');
const transferred: HTMLCanvasElement[] = [];

beforeAll(() => {
  vi.stubGlobal('Worker', FakeWorker);
  HTMLCanvasElement.prototype.transferControlToOffscreen = function (this: HTMLCanvasElement) {
    if (transferred.includes(this)) throw new DOMException('already transferred', 'InvalidStateError');
    transferred.push(this);
    return { offscreenFor: this } as unknown as OffscreenCanvas;
  };
});

afterAll(() => {
  vi.unstubAllGlobals();
  delete (HTMLCanvasElement.prototype as Partial<HTMLCanvasElement>).transferControlToOffscreen;
});

afterEach(() => {
  cleanup();
  FakeWorker.all = [];
  useEditorStore.setState({ waveform: null, activeWaveformChannel: 'luma' });
});

const data = (luma: string, rgb = ''): WaveformData => ({
  width: 256,
  height: 256,
  luma,
  rgb,
  parade: '',
  vectorscope: ''
});

const pngs = (worker: FakeWorker) =>
  worker.messages.flatMap((m) => ((m as { png?: string }).png ? [(m as { png: string }).png] : []));

it('hands each new scope image to the worker drawing the canvas, once, without an <img>', () => {
  useEditorStore.setState({ waveform: data('A') });
  const { container } = render(<StudioScopes />);
  const canvas = container.querySelector('.studio-scope-plot > canvas') as HTMLCanvasElement;
  expect(canvas).not.toBeNull();
  expect(canvas.width).toBe(256);
  expect(container.querySelector('.studio-scope-plot > img')).toBeNull();
  expect(FakeWorker.all.length).toBe(1);
  const [worker] = FakeWorker.all;
  expect((worker.messages[0] as { canvas: { offscreenFor: HTMLCanvasElement } }).canvas.offscreenFor).toBe(canvas);
  act(() => useEditorStore.setState({ waveform: data('B') }));
  // The same image again (e.g. a histogram-only update) is not sent again.
  act(() => useEditorStore.setState({ waveform: data('B') }));
  expect(pngs(worker)).toEqual(['A', 'B']);
});

it('gives a mode switch a fresh canvas and worker, and ends the old one', async () => {
  useEditorStore.setState({ waveform: data('L', 'R') });
  const { container } = render(<StudioScopes />);
  const first = FakeWorker.all[0];
  fireEvent.click(screen.getByRole('button', { name: 'RGB' }));
  expect(FakeWorker.all.length).toBe(2);
  expect(pngs(FakeWorker.all[1])).toEqual(['R']);
  expect(container.querySelectorAll('.studio-scope-plot > canvas').length).toBe(1);
  await act(() => new Promise((r) => setTimeout(r)));
  expect(first.terminated).toBe(true);
  expect(FakeWorker.all[1].terminated).toBe(false);
});

it('falls back to an <img> when the worker fails to start', () => {
  useEditorStore.setState({ waveform: data('A') });
  const { container } = render(<StudioScopes />);
  act(() => FakeWorker.all[0].listeners.error());
  expect(container.querySelector('.studio-scope-plot > canvas')).toBeNull();
  expect(container.querySelector('.studio-scope-plot > img')?.getAttribute('src')).toBe('data:image/png;base64,A');
});
