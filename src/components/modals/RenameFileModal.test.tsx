import '../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import RenameFileModal, { PREVIEW_ROWS, type RenamePlan } from './RenameFileModal';
import { Invokes } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

afterEach(cleanup);
beforeEach(() => vi.mocked(invoke).mockReset());

const photo = (from: string, to: string, extra: Partial<RenamePlan['photos'][number]> = {}) => ({
  from,
  to,
  files: from === to ? [] : [{ from, to, role: 'photo' as const }],
  notes: [],
  problems: [],
  ...extra,
});

function open(paths: string[], plan: RenamePlan) {
  vi.mocked(invoke).mockResolvedValue(plan);
  const onSave = vi.fn();
  const onClose = vi.fn();
  render(<RenameFileModal filesToRename={paths} isOpen onClose={onClose} onSave={onSave} />);
  return { onSave, onClose };
}

it('previews the final names, sidecars and notes and saves exactly that plan', async () => {
  const plan: RenamePlan = {
    photos: [
      photo('/s/A.NEF', '/s/Job_1.NEF', {
        files: [
          { from: '/s/A.NEF', to: '/s/Job_1.NEF', role: 'photo' },
          { from: '/s/A.JPG', to: '/s/Job_1.JPG', role: 'companion' },
          { from: '/s/A.xmp', to: '/s/Job_1.xmp', role: 'xmp' },
        ],
      }),
      photo('/s/B.NEF', '/s/Job_2.NEF', {
        files: [
          { from: '/s/B.NEF', to: '/s/Job_2.NEF', role: 'photo' },
          { from: '/s/B.xmp', to: '/s/Job_2.xmp', role: 'xmp' },
        ],
        notes: [{ kind: 'xmpWithRaw', xmp: '/s/B.xmp', others: ['/s/B.JPG'] }],
      }),
    ],
  };
  const { onSave } = open(['/s/A.NEF', '/s/B.NEF'], plan);
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'Job' } });
  const list = await screen.findByTestId('rename-preview');
  expect(invoke).toHaveBeenLastCalledWith(Invokes.PreviewRename, {
    paths: ['/s/A.NEF', '/s/B.NEF'],
    nameTemplate: 'Job',
    refresh: true,
  });
  const rows = within(list).getAllByRole('listitem');
  expect(rows[0].textContent).toContain('A.NEF→Job_1.NEF');
  expect(rows[0].textContent).toContain('Also renamed: Job_1.JPG, Job_1.xmp');
  expect(rows[1].textContent).toContain('B.xmp is renamed with this RAW; B.JPG keeps its name without it.');
  expect(screen.getByText('Photos renamed: 2 · files in all: 5')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'Save' }));
  expect(onSave).toHaveBeenCalledWith('Job', [...plan.photos[0].files, ...plan.photos[1].files]);
  // Later templates reuse the facts read for this opening.
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'Job2' } });
  await waitFor(() =>
    expect(invoke).toHaveBeenLastCalledWith(Invokes.PreviewRename, {
      paths: ['/s/A.NEF', '/s/B.NEF'],
      nameTemplate: 'Job2',
      refresh: false,
    }),
  );
});

it('shows why a name cannot be taken and does not save', async () => {
  const { onSave } = open(['/s/A.NEF'], {
    photos: [photo('/s/A.NEF', '/s/B.NEF', { problems: [{ kind: 'occupied', path: '/s/B.NEF' }] })],
  });
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'B' } });
  expect(await screen.findByText('B.NEF already exists.')).toBeTruthy();
  expect(screen.getByText('Nothing can be renamed until every problem above is solved.')).toBeTruthy();
  const save = screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement;
  expect(save.disabled).toBe(true);
  fireEvent.keyDown(screen.getByRole('textbox'), { key: 'Enter' });
  expect(onSave).not.toHaveBeenCalled();
});

it('lists the first rows of a large batch and counts the rest', async () => {
  const paths = Array.from({ length: PREVIEW_ROWS + 7 }, (_, i) => `/s/P${i}.NEF`);
  open(paths, { photos: paths.map((p, i) => photo(p, `/s/Job_${i}.NEF`)) });
  const list = await screen.findByTestId('rename-preview');
  expect(within(list).getAllByRole('listitem')).toHaveLength(PREVIEW_ROWS);
  expect(screen.getByText('More photos not listed: 7')).toBeTruthy();
});

it('cannot save when no name changes', async () => {
  open(['/s/A.NEF', '/s/B.NEF'], { photos: [photo('/s/A.NEF', '/s/A.NEF'), photo('/s/B.NEF', '/s/B.NEF')] });
  expect(await screen.findByText('No name changes.')).toBeTruthy();
  expect(screen.getAllByText('keeps its name')).toHaveLength(2);
  await waitFor(() => expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(true));
});

it('always lists a refused photo beyond the first rows', async () => {
  const paths = Array.from({ length: PREVIEW_ROWS + 7 }, (_, i) => `/s/P${i}.NEF`);
  const photos = paths.map((p, i) => photo(p, `/s/Job_${i}.NEF`));
  photos[PREVIEW_ROWS + 3].problems = [{ kind: 'occupied', path: '/s/Taken.NEF' }];
  open(paths, { photos });
  const list = await screen.findByTestId('rename-preview');
  expect(within(list).getAllByRole('listitem')).toHaveLength(PREVIEW_ROWS + 1);
  expect(screen.getByText('Taken.NEF already exists.')).toBeTruthy();
  expect(screen.getByText('More photos not listed: 6')).toBeTruthy();
});
