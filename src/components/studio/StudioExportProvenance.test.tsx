import '../../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useEffect } from 'react';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import StudioExportProvenance, {
  provenanceReady,
  useProvenanceIdentity,
  type ProvenanceIdentity
} from './StudioExportProvenance';
import { DEFAULT_RECIPE_DRAFT, exportSettingsFromDraft, type RecipeDraft } from '../../utils/exportRecipes';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(() => Promise.resolve('/certs/me.p12')) }));

const local: ProvenanceIdentity = {
  kind: 'generated',
  signer: 'Studio Example',
  issuer: 'Studio Example (local certificate authority)',
  validUntil: '2031-11-21T00:00:00Z',
  algorithm: 'ES256',
  fingerprint: 'ab',
  expired: false
};

let stored: ProvenanceIdentity | null;

beforeEach(() => {
  stored = null;
  vi.mocked(invoke).mockImplementation((command: string, args?: unknown) => {
    switch (command) {
      case 'provenance_identity':
        return Promise.resolve(stored);
      case 'generate_provenance_identity':
        stored = { ...local, signer: (args as { signer: string }).signer };
        return Promise.resolve(stored);
      case 'remove_provenance_identity':
        stored = null;
        return Promise.resolve(true);
      case 'export_provenance_preview':
        return Promise.resolve({
          manifest: {
            generator: 'Rawtonic',
            generatorVersion: '0.1.0',
            ingredientTitle: 'DSC0001.ARW',
            ingredientFormat: 'image/x-sony-arw',
            actions: [
              { action: 'c2pa.opened', description: 'Opened the photograph' },
              {
                action: 'c2pa.edited',
                digitalSourceType:
                  'http://cv.iptc.org/newscodes/digitalsourcetype/compositeWithTrainedAlgorithmicMedia',
                description: 'Area filled by a generative model'
              }
            ]
          },
          identity: stored,
          formatError: null
        });
      default:
        return Promise.resolve(undefined);
    }
  });
});
afterEach(cleanup);

function Harness({ initial, onReady }: { initial: Partial<RecipeDraft>; onReady: (ready: boolean) => void }) {
  const draft = { ...DEFAULT_RECIPE_DRAFT, ...initial };
  const provenance = useProvenanceIdentity(draft.provenance);
  const ready = provenanceReady(draft, provenance.identity);
  useEffect(() => onReady(ready), [ready, onReady]);
  return (
    <StudioExportProvenance
      draft={draft}
      set={() => {}}
      disabled={false}
      path="/photos/DSC0001.ARW"
      signed={
        draft.provenance ? [{ key: 'current', label: 'Current settings', ...exportSettingsFromDraft(draft) }] : []
      }
      library={{ kind: 'noLibrary' }}
      identity={provenance.identity}
      setIdentity={provenance.setIdentity}
      loadError={provenance.error}
    />
  );
}

it('is off by default and asks nothing of the backend', () => {
  const ready = vi.fn();
  render(<Harness initial={{}} onReady={ready} />);
  expect((screen.getByRole('checkbox', { name: /provenance manifest/i }) as HTMLInputElement).checked).toBe(false);
  expect(invoke).not.toHaveBeenCalled();
  expect(ready).toHaveBeenLastCalledWith(true);
});

it('blocks the export until a certificate is set up, says a local one is an unknown signer, and shows the manifest', async () => {
  const ready = vi.fn();
  const { rerender } = render(<Harness initial={{ provenance: true }} onReady={ready} />);
  await screen.findByText('No signing certificate is set up.');
  expect(ready).toHaveBeenLastCalledWith(false);
  fireEvent.change(screen.getByLabelText('Signer name'), { target: { value: 'Studio Example' } });
  fireEvent.click(screen.getByRole('button', { name: 'Make a certificate on this computer' }));
  await screen.findByText(/Signed as “Studio Example” with a certificate made on this computer/);
  expect(invoke).toHaveBeenCalledWith('generate_provenance_identity', { signer: 'Studio Example' });
  expect(screen.getAllByText(/unknown signer/).length).toBeGreaterThan(0);
  await waitFor(() => expect(ready).toHaveBeenLastCalledWith(true));
  rerender(<Harness initial={{ provenance: true }} onReady={ready} />);
  // The review lists what goes in, AI areas marked as such.
  await screen.findByText('What goes into the manifest');
  expect(screen.getByText(/DSC0001\.ARW/)).toBeTruthy();
  expect(screen.getByText(/compositeWithTrainedAlgorithmicMedia/)).toBeTruthy();
  expect(screen.getByText('Not named')).toBeTruthy();
});

it('removes the certificate only after a second click', async () => {
  stored = local;
  render(<Harness initial={{ provenance: true }} onReady={() => {}} />);
  fireEvent.click(await screen.findByRole('button', { name: 'Remove signing certificate' }));
  expect(invoke).not.toHaveBeenCalledWith('remove_provenance_identity');
  fireEvent.click(screen.getByRole('button', { name: 'Remove' }));
  await screen.findByText('No signing certificate is set up.');
  expect(invoke).toHaveBeenCalledWith('remove_provenance_identity');
});

it('refuses a format that cannot carry a manifest', async () => {
  stored = local;
  const ready = vi.fn();
  render(<Harness initial={{ provenance: true, format: 'avif' }} onReady={ready} />);
  expect(screen.getByRole('alert').textContent).toContain('AVIF files cannot carry a provenance manifest yet');
  await screen.findByText(/Signed as/);
  expect(ready).toHaveBeenLastCalledWith(false);
});

it('blocks an expired certificate and says until when it was valid', async () => {
  stored = { ...local, kind: 'imported', expired: true, validUntil: '2020-01-02T00:00:00Z' };
  const ready = vi.fn();
  render(<Harness initial={{ provenance: true }} onReady={ready} />);
  expect((await screen.findByRole('alert')).textContent).toContain('not valid now');
  await waitFor(() => expect(ready).toHaveBeenLastCalledWith(false));
});
