import '../../i18n';
import i18n from 'i18next';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ProofSheetDialogHost } from './ProofSheetDialog';
import { useProofSheetDialog } from '../../utils/proofSheet';
import { useLibraryStore } from '../../store/useLibraryStore';
import { runAppCommand } from '../../utils/appCommands';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('../../hooks/useEditorActions', () => ({ flushRecipeSaves: vi.fn(async () => []) }));
vi.mock('../../utils/photoWrites', () => ({ photoWritesAndStepsSettled: vi.fn(async () => {}) }));

const LIBRARY = '/jobs/Wedding';
const photos = ['/jobs/Wedding/Capture/A.NEF', '/jobs/Wedding/Capture/B.NEF'];
const calls = (name: string) => vi.mocked(invoke).mock.calls.filter(([command]) => command === name);
const rect = (x: number, y: number) => ({ x, y, w: 40, h: 30 });

beforeEach(async () => {
  await i18n.changeLanguage('en');
  localStorage.clear();
  useLibraryStore.setState({
    sessionDirectory: LIBRARY,
    imageList: photos.map((path) => ({ path })) as never,
    multiSelectedPaths: []
  });
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === 'proof_sheet_preview')
      return {
        layout: {
          pageWidthMm: 210,
          pageHeightMm: 297,
          pages: [
            {
              cells: [
                { photo: 0, cell: rect(12, 20), photoBox: rect(12, 20), captionBaseline: 53 },
                { photo: 1, cell: rect(56, 20), photoBox: rect(56, 20), captionBaseline: 53 }
              ]
            }
          ],
          boxPx: [472, 354],
          headerBaseline: 15,
          footerBaseline: 290
        },
        cropMarks: [],
        captions: ['1 A.NEF', '2 B.NEF'],
        header: 'Wedding',
        footer: '2026-11-02',
        fileName: 'Wedding proof sheet'
      };
    if (name === 'session_usable_folder') return `${LIBRARY}/Output`;
    if (name === 'queue_proof_sheet') return 'job-1';
    return undefined;
  });
});

afterEach(() => {
  act(() => useProofSheetDialog.getState().close());
  cleanup();
  vi.clearAllMocks();
});

it('File › Proof Sheet shows page 1 as laid out and queues the sheet into the Output folder', async () => {
  render(<ProofSheetDialogHost />);
  act(() => {
    runAppCommand('proof_sheet');
  });
  expect(await screen.findByText('1 A.NEF')).toBeTruthy();
  expect(screen.getByText('Wedding')).toBeTruthy();
  expect(screen.getByText(/Photos: 2 · pages: 1 · 20 per page/)).toBeTruthy();

  fireEvent.change(screen.getByLabelText('Columns'), { target: { value: '5' } });
  await waitFor(() => {
    const last = calls('proof_sheet_preview').at(-1)![1] as { request: { sheet: { columns: number } } };
    expect(last.request.sheet.columns).toBe(5);
  });

  fireEvent.click(screen.getByRole('button', { name: 'Create' }));
  await waitFor(() => expect(calls('queue_proof_sheet')).toHaveLength(1));
  const { request } = calls('queue_proof_sheet')[0][1] as {
    request: { paths: string[]; destination: string; sheet: { columns: number }; library: unknown; jobName: string };
  };
  expect(request.paths).toEqual(photos);
  expect(request.destination).toBe(`${LIBRARY}/Output`);
  expect(request.sheet.columns).toBe(5);
  expect(request.library).toEqual({ library: LIBRARY });
  expect(request.jobName).toBe('Wedding');
  // Closed, and the settings come back next time for this library.
  await waitFor(() => expect(useProofSheetDialog.getState().paths).toBeNull());
  expect(JSON.parse(localStorage.getItem(`rawtonic.proofSheet.form:${LIBRARY}`)!).columns).toBe(5);
});

it('a layout the backend refuses is shown and cannot be created', async () => {
  vi.mocked(invoke).mockImplementation(async (name: string) => {
    if (name === 'proof_sheet_preview') throw 'The cells are too small: use fewer columns or rows, or smaller margins.';
    return undefined;
  });
  render(<ProofSheetDialogHost />);
  act(() => useProofSheetDialog.getState().show(photos));
  expect((await screen.findByRole('alert')).textContent).toContain('too small');
  expect((screen.getByRole('button', { name: 'Create' }) as HTMLButtonElement).disabled).toBe(true);
});

it('without an Export watermark, its text is typed in the sheet and kept in the Export form, not switched on there', async () => {
  render(<ProofSheetDialogHost />);
  act(() => useProofSheetDialog.getState().show(photos));
  expect(await screen.findByText('1 A.NEF')).toBeTruthy();
  const create = () => screen.getByRole('button', { name: 'Create' }) as HTMLButtonElement;

  fireEvent.click(screen.getByLabelText('Watermark'));
  // Ticked without text: nothing to print yet.
  expect(create().disabled).toBe(true);
  fireEvent.change(screen.getByLabelText('Watermark text'), { target: { value: 'PROOF' } });
  await waitFor(() => expect(create().disabled).toBe(false));

  fireEvent.click(create());
  await waitFor(() => expect(calls('queue_proof_sheet')).toHaveLength(1));
  const { request } = calls('queue_proof_sheet')[0][1] as { request: { watermark: { text: { text: string } } | null } };
  expect(request.watermark?.text.text).toBe('PROOF');
  const form = JSON.parse(localStorage.getItem(`rawtonic.export.form:${LIBRARY}`)!);
  expect([form.watermarkKind, form.watermarkText, form.watermark]).toEqual(['text', 'PROOF', false]);
  // A form made here keeps the Export dialog's own default: the Output folder.
  expect(form.where).toBe('output');
});
