import '../../i18n';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import ReviewSendDialog from './ReviewSendDialog';
import { reviewView, type PreparedContribution } from './relay';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.reject(new Error('no IPC in tests'))) }));

afterEach(cleanup);

const PNG = 'iVBORw0KGgo=';
const prepared = (body: Record<string, unknown>): PreparedContribution => ({
  channel: 'report',
  body: JSON.stringify(body, null, 2),
  digest: 'd'.repeat(64),
});

describe('ReviewSendDialog', () => {
  it('shows the exact body and sends only on Send, never on Enter', async () => {
    const send = vi.fn(() => Promise.resolve({ id: 'abcdefghijklmnopqrstuvwxyz', ref: '0123abcd' }));
    const onSent = vi.fn();
    const p = prepared({ schema: 1, text: 'Export stops at <path>.CR3' });
    render(<ReviewSendDialog isOpen title="Report a problem" prepared={p} onClose={() => {}} send={send} onSent={onSent} />);
    expect(screen.getByLabelText('What will be sent').textContent).toBe(p.body);
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Enter' });
    expect(send).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(screen.getByTestId('report-id').textContent).toBe('abcdefghijklmnopqrstuvwxyz'));
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith(p);
    expect(onSent).toHaveBeenCalledWith({ id: 'abcdefghijklmnopqrstuvwxyz', ref: '0123abcd' });
  });

  it('cancels without sending, also with Escape', () => {
    const send = vi.fn();
    const onClose = vi.fn();
    render(<ReviewSendDialog isOpen title="Report" prepared={prepared({ schema: 1 })} onClose={onClose} send={send} />);
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onClose).toHaveBeenCalledTimes(2);
    expect(send).not.toHaveBeenCalled();
  });

  it('names the reason when sending fails and keeps the body for another try', async () => {
    const send = vi.fn(() => Promise.reject('rate-limited'));
    render(<ReviewSendDialog isOpen title="Report" prepared={prepared({ schema: 1 })} onClose={() => {}} send={send} />);
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('Too many reports right now. Try again later.'));
    expect(screen.getByRole('button', { name: 'Send' })).toBeTruthy();
  });

  it('shows a screenshot as an image instead of its base64 data', () => {
    render(
      <ReviewSendDialog
        isOpen
        title="Report"
        prepared={prepared({ schema: 1, screenshot: { mime: 'image/png', data: PNG } })}
        onClose={() => {}}
        send={vi.fn()}
      />,
    );
    expect(screen.getByLabelText('What will be sent').textContent).not.toContain(PNG);
    expect(screen.getByRole('img').getAttribute('src')).toBe(`data:image/png;base64,${PNG}`);
  });
});

describe('ReviewSendDialog focus', () => {
  it('keeps Tab inside the dialog and makes the page behind it inert', () => {
    const outside = document.createElement('button');
    outside.textContent = 'Behind';
    document.body.appendChild(outside);
    outside.focus();
    render(<ReviewSendDialog isOpen title="Report" prepared={prepared({ schema: 1 })} onClose={() => {}} send={vi.fn()} />);
    const dialog = screen.getByRole('dialog');
    expect(dialog.contains(document.activeElement)).toBe(true);
    expect(outside.closest('[inert]')).not.toBeNull();
    const send = screen.getByRole('button', { name: 'Send' });
    send.focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(dialog.contains(document.activeElement)).toBe(true);
    outside.remove();
  });
});

describe('reviewView', () => {
  it('changes nothing but the screenshot data', () => {
    const body = JSON.stringify({ schema: 1, screenshot: { data: PNG, mime: 'image/png' }, text: 'data' }, null, 2);
    const view = reviewView(body);
    expect(view.text).toBe(body.replace(`"${PNG}"`, '"‹image, shown below›"'));
    expect(view.screenshot?.bytes).toBe(8);
    expect(reviewView('{"schema":1}').text).toBe('{"schema":1}');
    expect(reviewView('not json').text).toBe('not json');
  });
});
