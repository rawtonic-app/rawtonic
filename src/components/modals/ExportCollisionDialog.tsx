// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import Button from '../ui/Button';
import { overlayHost } from '../../utils/topLayer';
import { useExportCollisionStore, type CollisionAnswer } from '../../store/useExportCollisionStore';
import type { CollisionKind } from '../ui/ExportImportProperties';

const MAX_NAMES_PER_KIND = 5;

/** "If a file exists: Ask": one question per export run, before anything
 * renders. The counts say what is in the way (an earlier export that did not
 * change, one that did, any other file), a few names show which; the answer
 * holds for all of them. Escape or Cancel exports nothing. */
export default function ExportCollisionDialog() {
  const { t } = useTranslation();
  const pending = useExportCollisionStore((s) => s.pending);
  const answer = useExportCollisionStore((s) => s.answer);
  const keepBoth = useRef<HTMLButtonElement>(null);
  const dialog = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!pending) return;
    // Keep both overwrites nothing: it is the one Enter reaches.
    keepBoth.current?.focus({ preventScroll: true });
    // The question is modal: Escape cancels it and nothing behind it (the
    // Export dialog) sees the key, and Tab stays among its buttons, so the
    // question is never left open and out of reach.
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        answer(null);
      } else if (e.key === 'Tab') {
        const buttons = [...(dialog.current?.querySelectorAll('button') ?? [])];
        if (!buttons.length) return;
        const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
        const next = e.shiftKey
          ? at <= 0
            ? buttons.length - 1
            : at - 1
          : at < 0 || at === buttons.length - 1
            ? 0
            : at + 1;
        e.preventDefault();
        buttons[next].focus();
      }
    };
    document.addEventListener('keydown', onKey, true);
    return () => document.removeEventListener('keydown', onKey, true);
  }, [pending, answer]);
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
  return createPortal(
    <div className="studio-dialog-backdrop" role="dialog" aria-modal="true" aria-labelledby="export-collision-title">
      <div className="studio-dialog" ref={dialog}>
        <h2 className="studio-dialog-title" id="export-collision-title">
          {t('studio.export.collision.title')}
        </h2>
        <p className="m-0">{t('studio.export.collision.intro')}</p>
        <ul className="m-0 pl-4" data-testid="collision-kinds">
          {kinds
            .filter(([, , kind]) => kind.count > 0)
            .map(([key, label, kind]) => {
              const names = kind.names.slice(0, MAX_NAMES_PER_KIND);
              const remaining = kind.count - names.length;
              return (
                <li key={key}>
                  <strong>{label}</strong>
                  <div className="text-xs text-text-secondary break-words">
                    {names.join(', ')}
                    {remaining > 0 && ` ${t('studio.export.collision.more', { number: remaining })}`}
                  </div>
                </li>
              );
            })}
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
