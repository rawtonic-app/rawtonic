// SPDX-License-Identifier: AGPL-3.0-or-later
import { type ReactNode, useEffect, useMemo, useRef } from 'react';
import { open } from '@tauri-apps/plugin-shell';
import { X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { type Block, type Inline, parseMarkdown } from '../utils/markdown';

const openLink = (href: string) => {
  void open(href).catch(console.error);
};

function inline(nodes: Inline[]): ReactNode[] {
  return nodes.map((node, i) => {
    switch (node.kind) {
      case 'text':
        return node.text;
      case 'code':
        return <code key={i}>{node.text}</code>;
      case 'strong':
        return <strong key={i}>{inline(node.children)}</strong>;
      case 'em':
        return <em key={i}>{inline(node.children)}</em>;
      case 'link':
        // Links open in the browser; the webview never navigates.
        return (
          <a
            key={i}
            href={node.href}
            title={node.href}
            onClick={(event) => {
              event.preventDefault();
              openLink(node.href);
            }}
          >
            {inline(node.children)}
          </a>
        );
    }
  });
}

function blocks(nodes: Block[]): ReactNode[] {
  return nodes.map((node, i) => {
    switch (node.kind) {
      case 'heading': {
        // The dialog's title is the h2; notes start below it.
        const Tag = (['h3', 'h3', 'h4', 'h5', 'h6', 'h6'] as const)[node.level - 1];
        return <Tag key={i}>{inline(node.children)}</Tag>;
      }
      case 'paragraph':
        return <p key={i}>{inline(node.children)}</p>;
      case 'code':
        return <pre key={i}><code>{node.text}</code></pre>;
      case 'quote':
        return <blockquote key={i}>{blocks(node.children)}</blockquote>;
      case 'list': {
        const List = node.ordered ? 'ol' : 'ul';
        return <List key={i}>{node.items.map((item, j) => <li key={j}>{blocks(item)}</li>)}</List>;
      }
      case 'rule':
        return <hr key={i} />;
    }
  });
}

export function Markdown({ source }: { source: string }) {
  const parsed = useMemo(() => parseMarkdown(source), [source]);
  return <div className="app-markdown">{blocks(parsed)}</div>;
}

/** Release notes in a modal dialog: the update notice's "What's new" and the
 * CHANGELOG section shown after an update. */
export function ReleaseNotesDialog({ title, notes, releaseUrl, footer, onClose }: {
  title: string;
  notes: string;
  releaseUrl?: string;
  footer?: ReactNode;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current;
    // jsdom has no showModal; the dialog is then simply open.
    if (element && !element.open) {
      if (element.showModal) element.showModal();
      else element.setAttribute('open', '');
    }
    return () => element?.close?.();
  }, []);
  return (
    <dialog
      ref={dialog}
      className="app-notes-dialog"
      aria-labelledby="app-notes-title"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
    >
      <header>
        <h2 id="app-notes-title">{title}</h2>
        <button type="button" className="app-notice-close" aria-label={t('updates.close')} onClick={onClose}>
          <X size={14} aria-hidden />
        </button>
      </header>
      <div className="app-notes-body" tabIndex={0}>
        {notes.trim() ? <Markdown source={notes} /> : <p className="app-notes-empty">{t('updates.noNotes')}</p>}
      </div>
      <footer>
        {footer}
        {releaseUrl && (
          <button type="button" onClick={() => openLink(releaseUrl)}>{t('updates.releasePage')}</button>
        )}
        <button type="button" className="is-primary" autoFocus onClick={onClose}>{t('updates.close')}</button>
      </footer>
    </dialog>
  );
}
