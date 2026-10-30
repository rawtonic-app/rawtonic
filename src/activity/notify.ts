// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * The app's one way to tell the user something, shaped like the toast API
 * it replaces so every call site keeps its words:
 *
 * - `error` and `warn` go to the activity centre's Needs attention, where
 *   they stay until resolved or dismissed: a failure is never only a toast
 *   that vanishes, and a background failure never opens a dialog.
 * - `success` and `info` confirm the user's own action: a short toast
 *   (about 3 s), also logged under Done.
 *
 * `toastId` is the entry's id, so a later notice under it replaces the
 * earlier one and `dismiss` settles it.
 */
import { isValidElement, type ReactNode } from 'react';
import i18n from 'i18next';
import { toast, type Id, type ToastOptions } from 'react-toastify';
import {
  useActivityStore,
  type ActivityAction,
  type ActivityBody,
  type ActivitySource,
} from '../store/useActivityStore';

/** A confirmation's time on screen. */
export const CONFIRMATION_MS = 3000;

export interface NotifyOptions extends Pick<ToastOptions, 'toastId' | 'autoClose' | 'closeOnClick'> {
  source?: ActivitySource;
  /** The plain text of rich content (else read from its elements). */
  title?: string;
  detail?: string;
  actions?: ActivityAction[];
  paths?: string[];
}

/** The words of a notice: its text, without the labels of its buttons. */
export function plainText(node: unknown): string {
  if (node == null || typeof node === 'boolean') return '';
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(plainText).join('');
  if (isValidElement(node)) {
    if (node.type === 'button') return '';
    const props = node.props as { children?: unknown };
    const text = plainText(props.children);
    // Block elements read as separate lines.
    return typeof node.type === 'string' && ['p', 'div', 'li'].includes(node.type) ? `${text}\n` : text;
  }
  return '';
}

function textOf(content: ActivityBody): string {
  if (typeof content === 'function') {
    try {
      return plainText(content({ closeToast: () => {} })).trim();
    } catch {
      return '';
    }
  }
  return plainText(content).replace(/\n+/g, ' ').trim();
}

/** The entry's one line: the caller's, the content's words, else a
 * generic one (never "[object Object]"). */
function titleOf(content: ActivityBody, options?: NotifyOptions): string {
  if (options?.title) return options.title;
  if (typeof content === 'string' || typeof content === 'number') return String(content);
  return textOf(content) || i18n.t('activity.notice');
}

function attention(status: 'failure' | 'warning', content: ActivityBody, options?: NotifyOptions): Id {
  const plain = typeof content === 'string' || typeof content === 'number';
  const title = titleOf(content, options);
  const source = options?.source ?? 'app';
  // The same failure again (an autosave retried) is one entry, renewed.
  const same = useActivityStore
    .getState()
    .entries.find((entry) => entry.status === status && entry.source === source && entry.title === title);
  return useActivityStore.getState().report(status, {
    id: options?.toastId != null ? String(options.toastId) : same?.id,
    source,
    title,
    detail: options?.detail,
    body: plain ? undefined : content,
    actions: options?.actions,
    paths: options?.paths,
  });
}

function confirmation(kind: 'success' | 'info', content: ActivityBody, options?: NotifyOptions): Id {
  const plain = typeof content === 'string' || typeof content === 'number';
  const id = useActivityStore.getState().report('done', {
    id: options?.toastId != null ? String(options.toastId) : undefined,
    source: options?.source ?? 'app',
    title: titleOf(content, options),
    detail: options?.detail,
    body: plain ? undefined : content,
    actions: options?.actions,
    paths: options?.paths,
  });
  const autoClose = typeof options?.autoClose === 'number' ? options.autoClose : CONFIRMATION_MS;
  toast[kind](content as ReactNode, { closeOnClick: options?.closeOnClick, toastId: id, autoClose });
  return id;
}

export const notify = {
  error: (content: ActivityBody, options?: NotifyOptions) => attention('failure', content, options),
  warn: (content: ActivityBody, options?: NotifyOptions) => attention('warning', content, options),
  warning: (content: ActivityBody, options?: NotifyOptions) => attention('warning', content, options),
  success: (content: ActivityBody, options?: NotifyOptions) => confirmation('success', content, options),
  info: (content: ActivityBody, options?: NotifyOptions) => confirmation('info', content, options),
  /** Settles a notice: its toast and its entry go. */
  dismiss(id?: Id) {
    if (id == null) return;
    toast.dismiss(id);
    useActivityStore.getState().resolve(String(id));
  },
  isActive: (id: Id) => useActivityStore.getState().entries.some((entry) => entry.id === String(id)),
  /** Changes a notice's content in place. */
  update(id: Id, { render }: { render: ActivityBody }) {
    const plain = typeof render === 'string' || typeof render === 'number';
    useActivityStore.getState().update(String(id), { title: titleOf(render), body: plain ? undefined : render });
  },
};
