// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useRef, type KeyboardEvent } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import Button from '../ui/Button';
import { overlayHost } from '../../utils/topLayer';
import { useExportCollisionStore, type CollisionAnswer } from '../../store/useExportCollisionStore';
import type { CollisionKind } from '../ui/ExportImportProperties';

/** "If a file exists: Ask": one question per export run, before anything
 * renders. The counts say what is in the way (an earlier export that did not
 * change, one that did, any other file), a few names show which; the answer
 * holds for all of them. Escape or Cancel exports nothing. */
export default function ExportCollisionDialog() {
  const { t } = useTranslation();
  const pending = useExportCollisionStore((s) => s.pending);
  const answer = useExportCollisionStore((s) => s.answer);
  const keepBoth = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    // Keep both overwrites nothing: it is the one Enter reaches.
    if (pending) keepBoth.current?.focus({ preventScroll: true });
  }, [pending]);
  if (!pending) return null;
  const { summary } = pending;
  const ANSWERS: [CollisionAnswer, string, string][] = [
    ['keepBoth', t('studio.export.collision.choice.keepBoth'), t('studio.export.collision.hint.keepBoth')],
    ['skip', t('studio.export.collision.choice.skip'), t('studio.export.collision.hint.skip')],
    [
      'replaceIfChanged',
      t('studio.export.collision.choice.replaceIfChanged'),
      t('studio.export.collision.hint.replaceIfChanged')
    ]
  ];
  const kinds: [string, string, CollisionKind][] = [
    [
      'unchangedOwn',
      t('studio.export.collision.kind.unchangedOwn', { number: summary.unchangedOwn.count }),
      summary.unchangedOwn
    ],
    [
      'changedOwn',
      t('studio.export.collision.kind.changedOwn', { number: summary.changedOwn.count }),
      summary.changedOwn
    ],
    ['other', t('studio.export.collision.kind.other', { number: summary.other.count }), summary.other]
  ];
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      answer(null);
    }
  };
  return createPortal(
    <div className="studio-dialog-backdrop" role="dialog" aria-modal="true" aria-labelledby="export-collision-title">
      <div className="studio-dialog" onKeyDown={onKeyDown}>
        <h2 className="studio-dialog-title" id="export-collision-title">
          {t('studio.export.collision.title')}
        </h2>
        <p className="m-0">{t('studio.export.collision.intro')}</p>
        <ul className="m-0 pl-4" data-testid="collision-kinds">
          {kinds
            .filter(([, , kind]) => kind.count > 0)
            .map(([key, label, kind]) => (
              <li key={key}>
                <strong>{label}</strong>
                <div className="text-xs text-text-secondary">
                  {kind.names.join(', ')}
                  {kind.count > kind.names.length &&
                    ` ${t('studio.export.collision.more', { number: kind.count - kind.names.length })}`}
                </div>
              </li>
            ))}
        </ul>
        {summary.originals > 0 && (
          <p className="m-0">{t('studio.export.collision.originals', { number: summary.originals })}</p>
        )}
        <ul className="m-0 pl-4 text-xs text-text-secondary">
          {ANSWERS.map(([choice, label, hint]) => (
            <li key={choice}>
              <strong>{label}:</strong> {hint}
            </li>
          ))}
        </ul>
        <div className="studio-dialog-footer">
          <Button variant="ghost" onClick={() => answer(null)}>
            {t('studio.export.collision.cancel')}
          </Button>
          {ANSWERS.map(([choice, label]) => (
            <Button
              key={choice}
              ref={choice === 'keepBoth' ? keepBoth : undefined}
              variant={choice === 'keepBoth' ? 'primary' : 'ghost'}
              onClick={() => answer(choice)}
            >
              {label}
            </Button>
          ))}
        </div>
      </div>
    </div>,
    overlayHost()
  );
}
