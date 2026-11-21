import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../ui/AppProperties';
import { openDialog } from '../../utils/fileDialog';
import { provenanceFormat, type RecipeDraft } from '../../utils/exportRecipes';
import type { ExportSettings } from '../ui/ExportImportProperties';
import { currentEditForExport, useEditorStore } from '../../store/useEditorStore';

/** The signing identity (`c2pa::identity::IdentityInfo`); never key material. */
export interface ProvenanceIdentity {
  kind: 'generated' | 'imported';
  signer: string;
  issuer: string;
  validUntil: string;
  algorithm: string;
  fingerprint: string;
  /** Not valid now: exports cannot be signed with it. */
  expired: boolean;
}

/**
 * The signing identity, read once an export wants provenance (`undefined`
 * until read, `null` for none). The Export panel holds it: its section shows
 * and changes it, and every planned recipe is checked against it.
 */
export function useProvenanceIdentity(wanted: boolean) {
  const [identity, setIdentity] = useState<ProvenanceIdentity | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!wanted || identity !== undefined) return;
    let active = true;
    invoke<ProvenanceIdentity | null>(Invokes.ProvenanceIdentity)
      .then((info) => active && setIdentity(info ?? null))
      .catch((e) => {
        if (!active) return;
        setIdentity(null);
        setError(String(e));
      });
    return () => {
      active = false;
    };
  }, [wanted, identity]);
  return { identity, setIdentity, error };
}

/** Whether an export of `draft` can be signed with `identity` (the panel's preflight; the queue checks again). */
export function provenanceReady(draft: RecipeDraft, identity: ProvenanceIdentity | null | undefined): boolean {
  return !draft.provenance || (!!identity && !identity.expired && provenanceFormat(draft.format));
}

interface ManifestAction {
  action: string;
  digitalSourceType?: string;
  description?: string;
}

/** What one photo's manifest holds (`c2pa::ManifestPlan`). */
interface ProvenancePreview {
  manifest: {
    generator: string;
    generatorVersion: string;
    ingredientTitle: string;
    ingredientFormat?: string;
    actions: ManifestAction[];
    author?: string;
  };
  identity: ProvenanceIdentity | null;
  formatError: string | null;
}

interface Props {
  draft: RecipeDraft;
  set: (patch: Partial<RecipeDraft>) => void;
  disabled: boolean;
  /** The first photo to export: its manifest is shown. */
  path: string | null;
  settings: ExportSettings;
  library: unknown;
  /** The signing identity (`useProvenanceIdentity`). */
  identity: ProvenanceIdentity | null | undefined;
  setIdentity: (identity: ProvenanceIdentity | null) => void;
  /** Why the identity could not be read. */
  loadError: string | null;
}

const date = (rfc3339: string) => {
  const parsed = new Date(rfc3339);
  return Number.isNaN(parsed.getTime()) ? rfc3339 : parsed.toLocaleDateString();
};

/**
 * The Export panel's Provenance (C2PA) section: off by default; signs each
 * JPEG, PNG or TIFF with the user's certificate or one made on this computer, and
 * shows what the manifest of the first photo holds before anything is
 * exported. Nothing is sent anywhere.
 */
export default function StudioExportProvenance({
  draft,
  set,
  disabled,
  path,
  settings,
  library,
  identity,
  setIdentity,
  loadError
}: Props) {
  const { t } = useTranslation();
  const [actionError, setError] = useState<string | null>(null);
  const error = actionError ?? loadError;
  const [working, setWorking] = useState(false);
  const [signer, setSigner] = useState('');
  const [importing, setImporting] = useState<{ file: string; password: string } | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [preview, setPreview] = useState<{ request: string; preview: ProvenancePreview } | null>(null);

  const formatOk = provenanceFormat(draft.format);

  const request = JSON.stringify({
    path,
    exportSettings: settings,
    outputFormat: draft.format,
    library,
    identity: identity?.fingerprint ?? null
  });
  useEffect(() => {
    const parsed = JSON.parse(request) as { path: string | null };
    if (!draft.provenance || !parsed.path) return;
    let stale = false;
    const timer = setTimeout(() => {
      // `identity` only asks for the review again when the signer changes.
      const args = JSON.parse(request);
      delete args.identity;
      invoke<ProvenancePreview>(Invokes.ExportProvenancePreview, {
        ...args,
        ...currentEditForExport(useEditorStore.getState())
      })
        .then((result) => !stale && result && setPreview({ request, preview: result }))
        .catch(() => {
          // The review is shown again with the next change.
        });
    }, 250);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [request, draft.provenance]);
  const shown = preview?.request === request ? preview.preview : null;

  const run = async (action: () => Promise<ProvenanceIdentity | boolean>) => {
    setWorking(true);
    setError(null);
    try {
      const result = await action();
      setIdentity(typeof result === 'boolean' ? null : result);
      setImporting(null);
      setConfirmRemove(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };
  const generate = () =>
    run(() => invoke<ProvenanceIdentity>(Invokes.GenerateProvenanceIdentity, { signer: signer.trim() }));
  const chooseCertificate = async () => {
    const file = await openDialog({
      title: t('studio.export.provenance.importTitle'),
      purpose: 'certificate',
      multiple: false,
      directory: false,
      filters: [{ name: t('studio.export.provenance.certificateFilter'), extensions: ['p12', 'pfx'] }]
    });
    if (typeof file === 'string') setImporting({ file, password: '' });
  };
  const importCertificate = () =>
    importing &&
    run(() =>
      invoke<ProvenanceIdentity>(Invokes.ImportProvenanceCertificate, {
        path: importing.file,
        password: importing.password
      })
    );
  const remove = () => run(() => invoke<boolean>(Invokes.RemoveProvenanceIdentity).then(() => false));

  return (
    <details className="studio-export-section">
      <summary>{t('studio.export.provenance.legend')}</summary>
      <fieldset disabled={disabled}>
        <legend>{t('studio.export.provenance.legend')}</legend>
        <label className="studio-check">
          <input type="checkbox" checked={draft.provenance} onChange={(e) => set({ provenance: e.target.checked })} />
          {t('studio.export.provenance.add')}
        </label>
        <small>{t('studio.export.provenance.about')}</small>
        {draft.provenance && (
          <>
            {!formatOk && (
              <p className="studio-export-error" role="alert">
                {t('studio.export.provenance.formatUnsupported', { format: draft.format.toUpperCase() })}
              </p>
            )}
            <label className="studio-check">
              <input
                type="checkbox"
                checked={draft.provenanceAuthor}
                onChange={(e) => set({ provenanceAuthor: e.target.checked })}
              />
              {t('studio.export.provenance.author')}
            </label>

            <div className="studio-provenance-identity" aria-live="polite">
              {identity === undefined ? null : identity ? (
                <>
                  <p>
                    {identity.kind === 'generated'
                      ? t('studio.export.provenance.signedLocal', { signer: identity.signer })
                      : t('studio.export.provenance.signedImported', {
                          signer: identity.signer,
                          issuer: identity.issuer,
                          until: date(identity.validUntil)
                        })}
                  </p>
                  {identity.expired && (
                    <p className="studio-export-error" role="alert">
                      {t('studio.export.provenance.expired', { until: date(identity.validUntil) })}
                    </p>
                  )}
                  {identity.kind === 'generated' && <small>{t('studio.export.provenance.unknownSigner')}</small>}
                  {confirmRemove ? (
                    <div role="group" aria-label={t('studio.export.provenance.remove')}>
                      <small>{t('studio.export.provenance.removeConfirm')}</small>
                      <button type="button" disabled={working} onClick={() => void remove()}>
                        {t('studio.export.provenance.removeYes')}
                      </button>
                      <button type="button" onClick={() => setConfirmRemove(false)}>
                        {t('studio.export.provenance.removeNo')}
                      </button>
                    </div>
                  ) : (
                    <button type="button" onClick={() => setConfirmRemove(true)}>
                      {t('studio.export.provenance.remove')}
                    </button>
                  )}
                </>
              ) : (
                <>
                  <p>{t('studio.export.provenance.none')}</p>
                  <label>
                    {t('studio.export.provenance.signerName')}
                    <input
                      type="text"
                      value={signer}
                      placeholder={t('studio.export.provenance.signerPlaceholder')}
                      onChange={(e) => setSigner(e.target.value)}
                    />
                  </label>
                  <small>{t('studio.export.provenance.unknownSigner')}</small>
                  <button type="button" disabled={working} onClick={() => void generate()}>
                    {t('studio.export.provenance.generate')}
                  </button>
                  {importing ? (
                    <>
                      <label>
                        {t('studio.export.provenance.password')}
                        <input
                          type="password"
                          autoComplete="off"
                          value={importing.password}
                          onChange={(e) => setImporting({ ...importing, password: e.target.value })}
                        />
                      </label>
                      <small>{t('studio.export.provenance.keyStored')}</small>
                      <button type="button" disabled={working} onClick={() => void importCertificate()}>
                        {t('studio.export.provenance.importConfirm')}
                      </button>
                      <button type="button" onClick={() => setImporting(null)}>
                        {t('studio.export.provenance.removeNo')}
                      </button>
                    </>
                  ) : (
                    <button type="button" disabled={working} onClick={() => void chooseCertificate()}>
                      {t('studio.export.provenance.import')}
                    </button>
                  )}
                </>
              )}
              {error && (
                <p className="studio-export-error" role="alert">
                  {error}
                </p>
              )}
            </div>

            {shown && (
              <section className="studio-provenance-review" aria-label={t('studio.export.provenance.reviewLegend')}>
                <h5>{t('studio.export.provenance.reviewLegend')}</h5>
                <small>{t('studio.export.provenance.reviewHint')}</small>
                <dl>
                  <dt>{t('studio.export.provenance.review.generator')}</dt>
                  <dd>
                    {shown.manifest.generator} {shown.manifest.generatorVersion}
                  </dd>
                  <dt>{t('studio.export.provenance.review.signer')}</dt>
                  <dd>{shown.identity ? shown.identity.signer : t('studio.export.provenance.review.noSigner')}</dd>
                  <dt>{t('studio.export.provenance.review.ingredient')}</dt>
                  <dd>
                    {shown.manifest.ingredientTitle}
                    {shown.manifest.ingredientFormat ? ` (${shown.manifest.ingredientFormat})` : ''}
                  </dd>
                  <dt>{t('studio.export.provenance.review.author')}</dt>
                  <dd>{shown.manifest.author ?? t('studio.export.provenance.review.noAuthor')}</dd>
                  <dt>{t('studio.export.provenance.review.actions')}</dt>
                  <dd>
                    <ol>
                      {shown.manifest.actions.map((action, index) => (
                        <li key={index}>
                          <code>{action.action}</code>
                          {action.description ? ` — ${action.description}` : ''}
                          {action.digitalSourceType && (
                            <>
                              {' '}
                              <small>
                                {t('studio.export.provenance.review.generated', {
                                  type: action.digitalSourceType.split('/').pop()
                                })}
                              </small>
                            </>
                          )}
                        </li>
                      ))}
                    </ol>
                  </dd>
                  <dt>{t('studio.export.provenance.review.notIncluded')}</dt>
                  <dd>{t('studio.export.provenance.review.notIncludedList')}</dd>
                </dl>
              </section>
            )}
          </>
        )}
      </fieldset>
    </details>
  );
}
