import '../../i18n';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import CameraHelpDialog, { RAW_PIXLS_LIST_URL, RAW_PIXLS_URL, isDecoderError } from './CameraHelpDialog';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.reject(new Error('no IPC in tests'))) }));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn() }));

afterEach(cleanup);

const photo = { make: 'SONY', model: 'ILCE-7M5', fileType: 'ARW' };

describe('camera help', () => {
  it('guides to raw.pixls.us in the browser and sends nothing itself', async () => {
    const openUrl = vi.fn((_url: string) => Promise.resolve());
    const camera = vi.fn((_path: string) => Promise.resolve(photo));
    render(
      <CameraHelpDialog
        request={{ path: '/Volumes/Card/DSC01234.ARW', reason: 'unreadable' }}
        onClose={() => {}}
        onReport={() => {}}
        openUrl={openUrl}
        camera={camera}
      />,
    );
    expect(screen.getByText(/can't open “DSC01234.ARW”/)).toBeTruthy();
    await screen.findByText('Camera: SONY ILCE-7M5 (ARW)');
    expect(camera).toHaveBeenCalledWith('/Volumes/Card/DSC01234.ARW');
    expect(screen.getByText(/sends nothing to raw.pixls.us/)).toBeTruthy();
    expect(screen.getByText(/keeps its metadata/)).toBeTruthy();
    expect(openUrl).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Open the camera list' }));
    fireEvent.click(screen.getByRole('button', { name: 'Open raw.pixls.us' }));
    await waitFor(() => expect(openUrl.mock.calls.map(([url]) => url)).toEqual([RAW_PIXLS_LIST_URL, RAW_PIXLS_URL]));
  });

  it('offers a problem report for the file instead, with its camera', async () => {
    const onReport = vi.fn();
    render(
      <CameraHelpDialog
        request={{ path: '/x/DSC01234.ARW', reason: 'preview' }}
        onClose={() => {}}
        onReport={onReport}
        camera={() => Promise.resolve(photo)}
      />,
    );
    expect(screen.getByText(/shows the preview the camera stored/)).toBeTruthy();
    await screen.findByText('Camera: SONY ILCE-7M5 (ARW)');
    fireEvent.click(screen.getByRole('button', { name: 'Report a problem with this file…' }));
    expect(onReport).toHaveBeenCalledWith(photo);
  });

  it('is offered for decoder errors only, not for a missing file or a format no open decoder reads', () => {
    expect(isDecoderError("Error: Unknown camera, model 'ILCE-7M5', make: 'SONY', mode: ''")).toBe(true);
    expect(isDecoderError('No decoder found')).toBe(true);
    expect(isDecoderError('Fallback read failed for /x/a.ARW: No such file or directory')).toBe(false);
    expect(
      isDecoderError(
        'Nikon High Efficiency RAW cannot be developed in Rawtonic: the format is patented and has no open decoder.',
      ),
    ).toBe(false);
  });
});
