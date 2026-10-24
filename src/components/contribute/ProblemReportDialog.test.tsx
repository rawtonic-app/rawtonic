import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import ProblemReportDialog from './ProblemReportDialog';
import type { ContributionChannel, PreparedContribution } from './relay';
import type { ReportContext, Screenshot } from './reports';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.reject(new Error('no IPC in tests'))) }));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn() }));

const context: ReportContext = {
  app: { version: '0.1.0-alpha.4', os: 'macos', osVersion: '26.5', arch: 'aarch64' },
  gpu: { vendor: 'apple', name: 'Apple M5 Pro' },
  logTail: 'opening <path>.NEF\nexport failed',
  screenshotAvailable: true,
};
const screenshot: Screenshot = { mime: 'image/jpeg', data: 'AAAA', width: 2, height: 1 };

// Stands in for the app's scrubbing: the body that is shown and sent.
const prepare = vi.fn((channel: ContributionChannel, payload: Record<string, unknown>) =>
  Promise.resolve<PreparedContribution>({ channel, body: JSON.stringify(payload, null, 2), digest: 'd'.repeat(64) }),
);

let clipboard: ReturnType<typeof vi.fn>;
beforeEach(() => {
  prepare.mockClear();
  clipboard = vi.fn((_text: string) => Promise.resolve());
  Object.defineProperty(navigator, 'clipboard', { value: { writeText: clipboard }, configurable: true });
});
afterEach(cleanup);

const props = {
  isOpen: true,
  onClose: () => {},
  context,
  screenshot,
  photo: { make: 'NIKON', model: 'Z 6', fileType: 'NEF' },
  prepare,
};

describe('Report a problem', () => {
  it('works without the relay: copy and GitHub use the prepared body, no Send', async () => {
    const openUrl = vi.fn((_url: string) => Promise.resolve());
    render(<ProblemReportDialog {...props} available={() => Promise.resolve(false)} openUrl={openUrl} />);
    await screen.findByText(/cannot send reports itself/);
    expect(screen.queryByRole('button', { name: 'Review and send…' })).toBeNull();
    const copy = screen.getByRole('button', { name: /Copy report/ }) as HTMLButtonElement;
    expect(copy.disabled).toBe(true);
    fireEvent.change(screen.getByPlaceholderText(/the export stops/), { target: { value: 'Export stops at photo 3' } });
    fireEvent.click(copy);
    await waitFor(() => expect(clipboard).toHaveBeenCalledTimes(1));
    const copied = clipboard.mock.calls[0][0] as string;
    expect(copied).toContain('Export stops at photo 3');
    expect(copied).toContain('**Photo:** NIKON Z 6 (NEF)');
    expect(copied).toContain('export failed');
    expect(prepare).toHaveBeenCalledWith('report', expect.objectContaining({ text: 'Export stops at photo 3' }));
    fireEvent.click(screen.getByRole('button', { name: /Open on GitHub/ }));
    await waitFor(() => expect(openUrl).toHaveBeenCalledTimes(1));
    expect(openUrl.mock.calls[0][0]).toMatch(/^https:\/\/github\.com\/rawtonic-app\/rawtonic\/issues\/new\?title=Problem/);
  });

  it('reviews exactly the chosen parts, goes back to change them, and sends only on Send', async () => {
    const send = vi.fn(() => Promise.resolve({ id: 'abcdefghijklmnopqrstuvwxyz', ref: '0123abcd' }));
    render(<ProblemReportDialog {...props} available={() => Promise.resolve(true)} send={send} />);
    const review = await screen.findByRole('button', { name: 'Review and send…' });
    fireEvent.change(screen.getByPlaceholderText(/the export stops/), { target: { value: 'Crops vanish' } });
    // Screenshot off by default; log on; the user turns the log off and the screenshot on.
    fireEvent.click(screen.getByRole('checkbox', { name: /Last lines of Rawtonic's log/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Screenshot of the window/ }));
    fireEvent.click(screen.getByRole('checkbox', { name: /Camera and file type/ }));
    fireEvent.click(review);
    const body = await screen.findByLabelText('What will be sent');
    const payload = prepare.mock.calls.at(-1)?.[1] as Record<string, unknown>;
    expect(payload).toEqual({
      schema: 1,
      app: context.app,
      gpu: context.gpu,
      text: 'Crops vanish',
      screenshot: { mime: 'image/jpeg', data: 'AAAA' },
    });
    expect(body.textContent).toContain('"text": "Crops vanish"');
    expect(send).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Change…' }));
    expect((screen.getByPlaceholderText(/the export stops/) as HTMLTextAreaElement).value).toBe('Crops vanish');
    fireEvent.click(screen.getByRole('button', { name: 'Review and send…' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Send' }));
    await waitFor(() => expect(screen.getByTestId('report-id').textContent).toBe('abcdefghijklmnopqrstuvwxyz'));
    expect(send).toHaveBeenCalledTimes(1);
    expect(screen.getByText(/write to hello@rawtonic\.cc with this id/)).toBeTruthy();
  });

  it('deletes a sent report on request', async () => {
    const remove = vi.fn(() => Promise.resolve());
    const { default: ReviewSendDialog } = await import('./ReviewSendDialog');
    render(
      <ReviewSendDialog
        isOpen
        title="Report"
        prepared={{ channel: 'report', body: '{"schema":1}', digest: 'd'.repeat(64) }}
        onClose={() => {}}
        send={() => Promise.resolve({ id: 'abcdefghijklmnopqrstuvwxyz' })}
        remove={remove}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Delete this report now' }));
    await screen.findByText('Deleted.');
    expect(remove).toHaveBeenCalledWith('abcdefghijklmnopqrstuvwxyz');
  });
});
