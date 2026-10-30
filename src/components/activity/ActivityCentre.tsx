// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { Check, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import {
  activityDetails,
  activitySections,
  useActivityStore,
  type ActivityEntry,
} from '../../store/useActivityStore';
import { showPhotos } from '../../activity/showPhotos';
import { registerAppCommands } from '../../utils/appCommands';
import './activity.css';

/** A ring filled to `fraction`, or turning when the progress is unknown. */
function Ring({ fraction }: { fraction: number | null }) {
  const r = 6;
  const length = 2 * Math.PI * r;
  return (
    <svg className={`activity-ring${fraction === null ? ' is-indeterminate' : ''}`} width="16" height="16" viewBox="0 0 16 16" aria-hidden>
      <circle className="activity-ring-track" cx="8" cy="8" r={r} />
      <circle
        className="activity-ring-value"
        cx="8"
        cy="8"
        r={r}
        strokeDasharray={length}
        strokeDashoffset={fraction === null ? length * 0.7 : length * (1 - Math.min(1, Math.max(0, fraction)))}
      />
    </svg>
  );
}

/** How far the running tasks with a known progress are, together. */
function runningFraction(running: ActivityEntry[]): number | null {
  const known = running.filter((entry) => entry.progress && entry.progress.total > 0);
  if (!known.length) return null;
  const total = known.reduce((sum, entry) => sum + entry.progress!.total, 0);
  return known.reduce((sum, entry) => sum + entry.progress!.done, 0) / total;
}

function Body({ entry }: { entry: ActivityEntry }) {
  const dismiss = useActivityStore((state) => state.dismiss);
  if (entry.body === undefined) return <span className="activity-entry-title">{entry.title}</span>;
  const body = entry.body;
  const content: ReactNode =
    typeof body === 'function' ? body({ closeToast: () => dismiss(entry.id) }) : body;
  return <div className="activity-entry-title">{content}</div>;
}

function Entry({ entry }: { entry: ActivityEntry }) {
  const { t } = useTranslation();
  const { dismiss, resolve } = useActivityStore.getState();
  const row = useRef<HTMLLIElement>(null);
  const [copied, setCopied] = useState(false);
  const attention = entry.status === 'failure' || entry.status === 'warning';
  const copy = () => {
    const rendered = row.current?.querySelector('.activity-entry-title')?.textContent ?? '';
    void navigator.clipboard
      ?.writeText(activityDetails(entry, rendered))
      .then(() => setCopied(true))
      .catch(() => {});
  };
  const percent = entry.progress && entry.progress.total > 0 ? Math.round((entry.progress.done / entry.progress.total) * 100) : null;
  return (
    <li ref={row} className={`activity-entry is-${entry.status}`}>
      <span className="activity-entry-mark" aria-hidden>
        {entry.status === 'running' ? (
          <Ring fraction={percent === null ? null : percent / 100} />
        ) : entry.status === 'done' ? (
          <Check size={13} />
        ) : (
          <span className={`activity-dot is-${entry.status}`} />
        )}
      </span>
      <div className="activity-entry-main">
        {attention && <span className="sr-only">{t(entry.status === "failure" ? "activity.status.failure" : "activity.status.warning")}: </span>}
        <Body entry={entry} />
        {entry.detail && <p className="activity-entry-detail">{entry.detail}</p>}
        {entry.status === 'running' && entry.progress && (
          <progress
            className="activity-entry-progress"
            max={Math.max(entry.progress.total, 1)}
            value={entry.progress.done}
            aria-label={t('activity.progress', { percent: percent ?? 0 })}
          />
        )}
        <div className="activity-entry-actions">
          {entry.actions?.map((action) => (
            <button
              key={action.label}
              type="button"
              onClick={() => {
                if (action.settles) resolve(entry.id);
                void Promise.resolve().then(() => action.run());
              }}
            >
              {action.label}
            </button>
          ))}
          {entry.paths && entry.paths.length > 0 && (
            <button type="button" onClick={() => showPhotos(entry.title, entry.paths!)}>
              {t('activity.showPhotos')}
            </button>
          )}
          {attention && (
            <button type="button" onClick={copy}>
              {copied ? t('activity.copied') : t('activity.copyDetails')}
            </button>
          )}
          {entry.status === 'running' && entry.cancel && (
            <button type="button" onClick={() => void Promise.resolve().then(() => entry.cancel?.())}>
              {t('activity.cancel')}
            </button>
          )}
        </div>
      </div>
      {entry.status !== 'running' && (
        <button
          type="button"
          className="activity-entry-dismiss"
          aria-label={t('activity.dismiss', { title: entry.title })}
          title={t('activity.dismissShort')}
          onClick={() => dismiss(entry.id)}
        >
          <X size={12} aria-hidden />
        </button>
      )}
    </li>
  );
}

function Section({ id, title, entries, empty, extra }: { id: string; title: string; entries: ActivityEntry[]; empty: string; extra?: ReactNode }) {
  return (
    <section className="activity-section" aria-labelledby={id}>
      <div className="activity-section-head">
        <h3 id={id}>{title}</h3>
        {extra}
      </div>
      {entries.length ? (
        <ul>{entries.map((entry) => <Entry key={entry.id} entry={entry} />)}</ul>
      ) : (
        <p className="activity-empty">{empty}</p>
      )}
    </section>
  );
}

/** Announces new tasks and results politely and new failures at once. */
function useAnnouncements(entries: ActivityEntry[]) {
  const seen = useRef(new Map<string, string>());
  const [polite, setPolite] = useState('');
  const [assertive, setAssertive] = useState('');
  useEffect(() => {
    const politeLines: string[] = [];
    const assertiveLines: string[] = [];
    const next = new Map<string, string>();
    for (const entry of entries) {
      const key = `${entry.status}:${entry.title}`;
      next.set(entry.id, key);
      if (seen.current.get(entry.id) === key) continue;
      if (entry.status === 'failure' || entry.status === 'warning') assertiveLines.push(entry.title);
      else if (seen.current.get(entry.id)?.split(':')[0] !== entry.status) politeLines.push(entry.title);
    }
    seen.current = next;
    if (politeLines.length) setPolite(politeLines.join('. '));
    if (assertiveLines.length) setAssertive(assertiveLines.join('. '));
  }, [entries]);
  return { polite, assertive };
}

/**
 * The top bar's activity slot: empty when idle; a progress ring, the count
 * and the leading task while work runs; a dot (amber warning, red failure)
 * until what needs attention is resolved or dismissed. It opens the
 * Activity popover: Needs attention, Running, Done.
 */
export default function ActivityCentre() {
  const { t } = useTranslation();
  const entries = useActivityStore((state) => state.entries);
  const open = useActivityStore((state) => state.open);
  const setOpen = useActivityStore((state) => state.setOpen);
  const clearDone = useActivityStore((state) => state.clearDone);
  const { attention, running, done } = useMemo(() => activitySections(entries), [entries]);
  const anchor = useRef<HTMLSpanElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const returnFocus = useRef<HTMLElement | null>(null);
  const [position, setPosition] = useState({ left: 8, top: 40 });
  const { polite, assertive } = useAnnouncements(entries);

  // Its key (⌥⌘A, keyboardUtils) and the View menu open and close it.
  useEffect(
    () =>
      registerAppCommands({
        show_activity: {
          run: () => setOpen((shown) => !shown),
        },
      }),
    [setOpen],
  );

  useLayoutEffect(() => {
    if (!open) return;
    const box = anchor.current?.getBoundingClientRect();
    const width = panel.current?.getBoundingClientRect().width ?? 360;
    if (box) setPosition({ left: Math.max(8, Math.min(box.left, window.innerWidth - width - 8)), top: box.bottom + 6 });
  }, [open]);

  useEffect(() => {
    if (!open) return;
    returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    panel.current?.focus({ preventScroll: true });
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent) {
        if (event.key !== 'Escape') return;
        // Only the popover closes, not a view behind it.
        event.preventDefault();
        event.stopPropagation();
        setOpen(false);
        (button.current ?? returnFocus.current)?.focus({ preventScroll: true });
        return;
      }
      if (panel.current?.contains(event.target as Node) || anchor.current?.contains(event.target as Node)) return;
      setOpen(false);
    };
    window.addEventListener('keydown', close, true);
    window.addEventListener('pointerdown', close, true);
    return () => {
      window.removeEventListener('keydown', close, true);
      window.removeEventListener('pointerdown', close, true);
    };
  }, [open, setOpen]);

  const failure = attention.some((entry) => entry.status === 'failure');
  const lead = running[0] ?? attention[0];
  const summary = [
    running.length ? t('activity.slot.running', { count: running.length }) : null,
    attention.length ? t('activity.slot.attention', { count: attention.length }) : null,
  ]
    .filter(Boolean)
    .join(' · ');

  return (
    <span ref={anchor} className="activity-slot">
      {lead && (
        <button
          ref={button}
          type="button"
          className="activity-slot-button"
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-label={`${t('activity.title')}: ${summary}`}
          title={`${summary}\n${lead.title}`}
          onClick={() => setOpen((shown) => !shown)}
        >
          {running.length > 0 && <Ring fraction={runningFraction(running)} />}
          {running.length > 1 && <span className="activity-slot-count">{running.length}</span>}
          <span className="activity-slot-text">{lead.title}</span>
          {attention.length > 0 && <span className={`activity-dot is-${failure ? 'failure' : 'warning'}`} />}
        </button>
      )}
      {/* Live regions without a role: the page's own status and alert
          elements stay the only ones of their role. */}
      <span className="sr-only" aria-live="polite" aria-atomic="true">{polite}</span>
      <span className="sr-only" aria-live="assertive" aria-atomic="true">{assertive}</span>
      {open && (
        <div
          ref={panel}
          className="activity-panel"
          role="dialog"
          aria-labelledby="activity-heading"
          tabIndex={-1}
          style={{ left: position.left, top: position.top }}
        >
          <div className="activity-panel-head">
            <h2 id="activity-heading">{t('activity.title')}</h2>
            <button
              type="button"
              className="activity-entry-dismiss"
              aria-label={t('activity.close')}
              onClick={() => {
                setOpen(false);
                (button.current ?? returnFocus.current)?.focus({ preventScroll: true });
              }}
            >
              <X size={13} aria-hidden />
            </button>
          </div>
          <Section id="activity-attention" title={t('activity.sections.attention')} entries={attention} empty={t('activity.empty.attention')} />
          <Section id="activity-running" title={t('activity.sections.running')} entries={running} empty={t('activity.empty.running')} />
          <Section
            id="activity-done"
            title={t('activity.sections.done')}
            entries={done}
            empty={t('activity.empty.done')}
            extra={done.length > 0 && (
              <button type="button" className="activity-clear" onClick={clearDone}>{t('activity.clearDone')}</button>
            )}
          />
        </div>
      )}
    </span>
  );
}
