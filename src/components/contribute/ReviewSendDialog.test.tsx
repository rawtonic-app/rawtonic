import '../../i18n';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import ReviewSendDialog from './ReviewSendDialog';
import { attachmentsView, type PreparedContribution } from './relay';

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
    const p = prepared({ schema: 2, text: 'Export stops at <path>.CR3' });
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
    render(<ReviewSendDialog isOpen title="Report" prepared={prepared({ schema: 2 })} onClose={onClose} send={send} />);
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onClose).toHaveBeenCalledTimes(2);
    expect(send).not.toHaveBeenCalled();
  });

  it('names the reason when sending fails and keeps the body for another try', async () => {
    const send = vi.fn(() => Promise.reject('rate-limited'));
    render(<ReviewSendDialog isOpen title="Report" prepared={prepared({ schema: 2 })} onClose={() => {}} send={send} />);
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe('Too many reports right now. Try again later.'));
    expect(screen.getByRole('button', { name: 'Send' })).toBeTruthy();
  });

  it('shows the body verbatim with the screenshot it announces as an image, and the photo file by size', () => {
    const p: PreparedContribution = {
      ...prepared({ schema: 2, screenshot: { mime: 'image/png', size: 8, sha256: 'a'.repeat(64) } }),
      attachments: [
        { slot: 'screenshot', size: 8, sha256: 'a'.repeat(64), data: PNG },
        { slot: 'photo', size: 25_000_000, sha256: 'b'.repeat(64), path: '/private/x.CR3' },
      ],
    };
    render(<ReviewSendDialog isOpen title="Report" prepared={p} onClose={() => {}} send={vi.fn()} />);
    expect(screen.getByLabelText('What will be sent').textContent).toBe(p.body);
    expect(screen.getByRole('img').getAttribute('src')).toBe(`data:image/png;base64,${PNG}`);
    expect(screen.getByText(/^Photo file, /)).toBeTruthy();
  });

  it('says when the upload of an attached file was not confirmed', async () => {
    const send = vi.fn(() => Promise.resolve({ id: 'abcdefghijklmnopqrstuvwxyz', notUploaded: ['photo' as const] }));
    render(<ReviewSendDialog isOpen title="Report" prepared={prepared({ schema: 2 })} onClose={() => {}} send={send} />);
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toMatch(/could not be confirmed/));
    expect(screen.getByTestId('report-id').textContent).toBe('abcdefghijklmnopqrstuvwxyz');
  });
});

describe('ReviewSendDialog focus', () => {
  it('keeps Tab inside the dialog and makes the page behind it inert', () => {
    const outside = document.createElement('button');
    outside.textContent = 'Behind';
    document.body.appendChild(outside);
    outside.focus();
    render(<ReviewSendDialog isOpen title="Report" prepared={prepared({ schema: 2 })} onClose={() => {}} send={vi.fn()} />);
    const dialog = screen.getByRole('dialog');
    expect(dialog.contains(document.activeElement)).toBe(true);
    expect(outside.closest('[inert]')).not.toBeNull();
    const send = screen.getByRole('button', { name: 'Send' });
    send.focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(dialog.contains(document.activeElement)).toBe(true);
    outside.remove();
  });

  it('gives the focus back to the control that opened it', () => {
    const opener = document.createElement('button');
    opener.textContent = 'Report a problem';
    document.body.appendChild(opener);
    opener.focus();
    const view = render(<ReviewSendDialog isOpen title="Report" prepared={prepared({ schema: 2 })} onClose={() => {}} send={vi.fn()} />);
    expect(document.activeElement?.textContent).toBe('Cancel');
    view.rerender(<ReviewSendDialog isOpen={false} title="Report" prepared={prepared({ schema: 2 })} onClose={() => {}} send={vi.fn()} />);
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });
});

describe('attachmentsView', () => {
  it('takes the screenshot type from the body and shows nothing without files', () => {
    const p = {
      ...prepared({ schema: 2, screenshot: { mime: 'image/jpeg', size: 8, sha256: 'a'.repeat(64) } }),
      attachments: [{ slot: 'screenshot' as const, size: 8, sha256: 'a'.repeat(64), data: PNG }],
    };
    expect(attachmentsView(p).screenshot).toEqual({ src: `data:image/jpeg;base64,${PNG}`, bytes: 8 });
    expect(attachmentsView(prepared({ schema: 2 }))).toEqual({});
  });
});
