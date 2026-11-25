// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode
} from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronRight, Plus, Tag } from 'lucide-react';
import { useShallow } from 'zustand/react/shallow';
import { notify } from '../../activity/notify';
import { useContextMenu } from '../../context/ContextMenuContext';
import { saveFailure, storedKeyword } from '../../hooks/useLibraryActions';
import { useLibraryStore } from '../../store/useLibraryStore';
import { keywordAction, type KeywordLabel } from '../../utils/appUndo';
import {
  addKeywordTo,
  declaredKeywordsFollow,
  deleteKeywordOn,
  deleteUnusedKeywords,
  makeKeyword,
  photosWithKeyword,
  renameKeywordOn,
  useLibraryKeywords
} from '../../utils/keywordOps';
import {
  buildKeywordTree,
  displayKeyword,
  isWithinKeyword,
  keywordParent,
  legacyHierarchy,
  movedKeyword,
  parseKeywordPath,
  typedKeyword,
  renameKeyword,
  resolveKeywordPath,
  KEYWORD_SEPARATOR,
  type KeywordNode
} from '../../utils/keywordTree';
import { swallowDragClick } from '../../utils/swallowClick';
import { createDragGhost } from './dragGhost';
import { TILE_DROP_EVENT, type TileDropDetail } from './tileDrag';
import { ToolGroup } from './StudioDevelop';

/** A change waiting for its confirmation. */
type Pending = {
  kind: 'rename' | 'merge' | 'move' | 'convert' | 'delete' | 'deleteUnused';
  from: string;
  to: string;
  count: number;
  /** Delete unused keywords: the paths without photographs. */
  unused?: string[];
};

const fold = (text: string) => text.toLowerCase();

/** The node at `path` (ignoring case), if the tree has it. */
export function findKeyword(tree: KeywordNode[], path: string): KeywordNode | null {
  for (const node of tree) {
    if (fold(node.path) === fold(path)) return node;
    if (isWithinKeyword(path, node.path)) return findKeyword(node.children, path);
  }
  return null;
}

/** What renaming `from` to the typed `text` does: nothing (same path), a
 * rename, or a merge into a keyword the library has. */
export function plannedRename(tree: KeywordNode[], from: string, text: string): Pending | null {
  const to = parseKeywordPath(text).join(KEYWORD_SEPARATOR);
  const node = findKeyword(tree, from);
  if (!to || to === from || !node) return null;
  // Into itself (`Animals` → `Animals|Pets`) is a rename of every keyword
  // below it, never a merge with itself.
  const target = fold(to) !== fold(from) && !isWithinKeyword(to, from) ? findKeyword(tree, to) : null;
  return { kind: target ? 'merge' : 'rename', from, to: target ? target.path : to, count: node.count };
}

/**
 * Lightroom's Keyword List, Capture One's Keyword Library: every keyword of
 * the library as a tree, each with how many photographs hold it or one
 * under it. A click filters the grid by the keyword (its children
 * included); a drag onto another keyword moves it under that one (onto the
 * list's empty part: to the top level); the menu renames (a name the
 * library has merges), moves to the top level and converts a keyword typed
 * as `a > b` before hierarchies into the hierarchy. Each change names how
 * many photographs it rewrites and waits for a confirmation.
 */
export function StudioKeywordList() {
  const { t } = useTranslation();
  const { showContextMenu } = useContextMenu();
  const { directory, chosen, selection, setFilter } = useLibraryStore(
    useShallow((s) => ({
      directory: s.sessionDirectory,
      chosen: s.filterCriteria.metadata?.keyword,
      selection: s.filterCriteria.metadata,
      setFilter: s.setFilterCriteria
    }))
  );
  const values = useLibraryKeywords();
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [renaming, setRenaming] = useState<string | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [busy, setBusy] = useState(false);
  const [drag, setDrag] = useState<{ path: string; target: string | null } | null>(null);
  const list = useRef<HTMLDivElement>(null);

  const tree = useMemo(() => buildKeywordTree(values), [values]);
  const empty = tree.length === 0;
  const known = useMemo(() => values.map((value) => value.value), [values]);
  // Keywords no photograph has (made without photographs, or left so).
  const unused = useMemo(() => {
    const out: string[] = [];
    const walk = (nodes: KeywordNode[]) =>
      nodes.forEach((node) => {
        if (node.count === 0) out.push(node.path);
        walk(node.children);
      });
    walk(tree);
    return out;
  }, [tree]);
  const [making, setMaking] = useState<{ parent: string | null } | null>(null);
  const filtered = (path: string) => chosen?.length === 1 && fold(chosen[0]) === fold(path);
  const filterBy = (path: string) => setFilter({ metadata: { ...selection, keyword: filtered(path) ? [] : [path] } });
  const toggle = (path: string) =>
    setOpen((previous) => {
      const next = new Set(previous);
      if (next.has(fold(path))) next.delete(fold(path));
      else next.add(fold(path));
      return next;
    });

  const run = async (change: Pending) => {
    const { from, to } = change;
    setBusy(true);
    try {
      if (change.kind === 'deleteUnused') {
        if (directory) await deleteUnusedKeywords(directory, change.unused ?? []);
        return;
      }
      const label: KeywordLabel = { kind: change.kind, keyword: displayKeyword(from) };
      // A filter on the old name follows it (a deleted keyword leaves it).
      const followed = chosen?.some((value) => isWithinKeyword(value, from));
      const filter =
        chosen && followed
          ? {
              before: chosen,
              after:
                change.kind === 'delete'
                  ? chosen.filter((value) => !isWithinKeyword(value, from))
                  : chosen.map((value) => renameKeyword(value, from, to))
            }
          : undefined;
      // One undo step: the photographs' keywords and the keywords made
      // without photographs that follow them; Undo takes the filter back.
      await keywordAction(label, async (record) => {
        const paths = await photosWithKeyword(directory, from);
        try {
          if (change.kind === 'delete') await deleteKeywordOn(paths, from, record);
          else await renameKeywordOn(paths, from, to, record);
        } finally {
          record({
            declared:
              (change.kind === 'delete'
                ? await declaredKeywordsFollow(directory, 'delete', from)
                : await declaredKeywordsFollow(directory, 'rename', from, to)) ?? undefined
          });
        }
        record({ filter });
      });
      if (filter) setFilter({ metadata: { ...selection, keyword: filter.after } });
      const parent = keywordParent(to);
      if (change.kind !== 'delete' && parent) setOpen((previous) => new Set([...previous, fold(parent)]));
    } catch (e) {
      notify.error(t('studio.library.keywordList.toasts.renameFailed', { error: saveFailure(e, []).message }));
    } finally {
      setBusy(false);
    }
  };

  // A new keyword, with or without a parent, is made without photographs.
  const make = async (parent: string | null, text: string) => {
    const typed = parseKeywordPath(text);
    if (!typed.length || !directory) return;
    const levels = [...(parent ? parseKeywordPath(parent) : []), ...typed];
    const path = resolveKeywordPath(levels, known);
    setBusy(true);
    try {
      await makeKeyword(directory, path);
      // Open the levels above the new keyword so it shows.
      const above: string[] = [];
      for (let at = keywordParent(path); at; at = keywordParent(at)) above.push(fold(at));
      setOpen((previous) => new Set([...previous, ...above]));
    } catch (e) {
      notify.error(t('studio.library.keywordList.toasts.createFailed', { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  // Photographs dragged from the grid onto a keyword get it (tileDrag.ts):
  // the dragged selection, virtual copies only when they are selected.
  const dropRef = useRef<(detail: TileDropDetail) => void>(() => {});
  dropRef.current = ({ target, paths }) => {
    if (!paths.length || busy) return;
    addKeywordTo(paths, storedKeyword(target), displayKeyword(target)).catch((e) =>
      notify.error(t('studio.library.keywordList.toasts.assignFailed', { error: saveFailure(e, paths).message }))
    );
  };
  useEffect(() => {
    const element = list.current;
    if (!element) return;
    const onDrop = (e: Event) => dropRef.current((e as CustomEvent<TileDropDetail>).detail);
    element.addEventListener(TILE_DROP_EVENT, onDrop);
    return () => element.removeEventListener(TILE_DROP_EVENT, onDrop);
  }, [empty]);

  const confirmText = (change: Pending) => {
    const names = { from: displayKeyword(change.from), to: displayKeyword(change.to) };
    if (change.kind === 'deleteUnused') return t('studio.library.keywordList.confirm.deleteUnused');
    const what =
      change.kind === 'delete'
        ? t('studio.library.keywordList.confirm.delete', names)
        : change.kind === 'merge'
          ? t('studio.library.keywordList.confirm.merge', names)
          : change.kind === 'move'
            ? t('studio.library.keywordList.confirm.move', names)
            : change.kind === 'convert'
              ? t('studio.library.keywordList.confirm.convert', names)
              : t('studio.library.keywordList.confirm.rename', names);
    return `${what} ${t('studio.library.keywordList.confirm.photos', { count: change.count })}`;
  };
  const actionLabel = (kind: Pending['kind']) =>
    kind === 'delete' || kind === 'deleteUnused'
      ? t('studio.library.keywordList.confirm.deleteAction')
      : kind === 'merge'
        ? t('studio.library.keywordList.confirm.mergeAction')
        : kind === 'move'
          ? t('studio.library.keywordList.confirm.moveAction')
          : kind === 'convert'
            ? t('studio.library.keywordList.confirm.convertAction')
            : t('studio.library.keywordList.confirm.renameAction');

  const menu = (e: ReactMouseEvent, node: KeywordNode) => {
    e.preventDefault();
    const legacy = legacyHierarchy(node.path);
    showContextMenu(e.clientX, e.clientY, [
      {
        label: filtered(node.path)
          ? t('studio.library.keywordList.menu.clearFilter')
          : t('studio.library.keywordList.menu.filter'),
        onClick: () => filterBy(node.path)
      },
      { label: t('studio.library.keywordList.menu.rename'), onClick: () => setRenaming(node.path) },
      {
        label: t('studio.library.keywordList.menu.newInside', { keyword: displayKeyword(node.path) }),
        onClick: () => setMaking({ parent: node.path })
      },
      {
        label: t('studio.library.keywordList.menu.delete'),
        onClick: () => setPending({ kind: 'delete', from: node.path, to: node.path, count: node.count })
      },
      ...(keywordParent(node.path) !== null
        ? [
            {
              label: t('studio.library.keywordList.menu.moveToTop'),
              onClick: () =>
                setPending({ kind: 'move', from: node.path, to: movedKeyword(node.path, null)!, count: node.count })
            }
          ]
        : []),
      ...(legacy
        ? [
            {
              label: t('studio.library.keywordList.menu.convert', { path: displayKeyword(legacy) }),
              onClick: () => {
                const target = findKeyword(tree, legacy);
                setPending({
                  kind: target ? 'merge' : 'convert',
                  from: node.path,
                  to: target?.path ?? legacy,
                  count: node.count
                });
              }
            }
          ]
        : [])
    ]);
  };

  const renameKeys = (e: KeyboardEvent<HTMLInputElement>, from: string) => {
    e.stopPropagation();
    if (e.key === 'Escape') return setRenaming(null);
    if (e.key !== 'Enter') return;
    e.preventDefault();
    const change = plannedRename(tree, from, e.currentTarget.value);
    setRenaming(null);
    if (change) setPending(change);
  };

  // A keyword dragged onto another moves under it (pointer events, as the
  // Albums tool drags albums into groups).
  const targetAt = (x: number, y: number): { target: string | null } | null => {
    const element = document.elementFromPoint(x, y);
    const row = element?.closest<HTMLElement>('[data-keyword-row]');
    if (row) return { target: row.dataset.keywordRow ?? null };
    return list.current?.contains(element) ? { target: null } : null;
  };
  const press = (e: ReactPointerEvent<HTMLElement>, node: KeywordNode) => {
    if (e.button !== 0 || busy) return;
    const start = { x: e.clientX, y: e.clientY };
    let moved = false;
    let ghost: ReturnType<typeof createDragGhost> | null = null;
    const move = (ev: PointerEvent) => {
      if (!moved && Math.hypot(ev.clientX - start.x, ev.clientY - start.y) < 5) return;
      moved = true;
      ghost ??= createDragGhost({ label: node.name, kind: 'keyword' });
      ghost.move(ev.clientX, ev.clientY);
      const at = targetAt(ev.clientX, ev.clientY);
      setDrag({ path: node.path, target: at ? at.target : null });
    };
    const end = (ev: PointerEvent) => {
      document.removeEventListener('pointermove', move, true);
      document.removeEventListener('pointerup', end, true);
      document.removeEventListener('pointercancel', end, true);
      setDrag(null);
      ghost?.remove();
      if (!moved || ev.type !== 'pointerup') return;
      swallowDragClick(false);
      const at = targetAt(ev.clientX, ev.clientY);
      if (!at) return;
      const to = movedKeyword(node.path, at.target);
      if (!to) return;
      const target = findKeyword(tree, to);
      setPending({ kind: target ? 'merge' : 'move', from: node.path, to: target?.path ?? to, count: node.count });
    };
    document.addEventListener('pointermove', move, true);
    document.addEventListener('pointerup', end, true);
    document.addEventListener('pointercancel', end, true);
  };

  const rows = (nodes: KeywordNode[], depth: number): ReactNode[] =>
    nodes.flatMap((node) => {
      const expanded = open.has(fold(node.path));
      const droppable = drag && drag.target === node.path && movedKeyword(drag.path, node.path) !== null;
      const legacy = legacyHierarchy(node.path) !== null;
      const row =
        renaming === node.path ? (
          <input
            key={`rename-${node.path}`}
            className="studio-metadata-input studio-album-name"
            style={{ marginLeft: depth * 14 }}
            aria-label={t('studio.library.keywordList.renameLabel', { keyword: displayKeyword(node.path) })}
            defaultValue={typedKeyword(node.path)}
            autoFocus
            onKeyDown={(e) => renameKeys(e, node.path)}
            onBlur={() => setRenaming(null)}
          />
        ) : (
          <div
            key={node.path}
            role="treeitem"
            aria-level={depth + 1}
            aria-expanded={node.children.length ? expanded : undefined}
            aria-selected={filtered(node.path)}
            data-keyword-row={node.path}
            data-album-drop={node.path}
            className={`studio-preset studio-album studio-keyword-row ${filtered(node.path) ? 'is-applied' : ''} ${
              drag?.path === node.path ? 'is-dragged' : ''
            }${droppable ? ' is-drop-into' : ''}`}
            style={{ paddingLeft: 4 + depth * 14 }}
            onPointerDown={(e) => press(e, node)}
            onContextMenu={(e) => menu(e, node)}
          >
            {node.children.length ? (
              <button
                type="button"
                className="studio-keyword-disclosure"
                aria-label={t(expanded ? 'studio.library.keywordList.collapse' : 'studio.library.keywordList.expand', {
                  keyword: node.name
                })}
                onClick={() => toggle(node.path)}
              >
                <ChevronRight size={11} className={expanded ? 'is-open' : ''} />
              </button>
            ) : (
              <span className="studio-keyword-disclosure" aria-hidden="true" />
            )}
            <button
              type="button"
              className="studio-keyword-name"
              title={
                legacy
                  ? t('studio.library.keywordList.legacyHint', { keyword: node.path })
                  : t('studio.library.keywordList.filterHint', { keyword: displayKeyword(node.path) })
              }
              onClick={() => filterBy(node.path)}
              onDoubleClick={() => setRenaming(node.path)}
            >
              <Tag size={11} />
              <span>{node.name}</span>
              <small>{node.count.toLocaleString()}</small>
            </button>
          </div>
        );
      return [row, ...(expanded ? rows(node.children, depth + 1) : [])];
    });

  // Filtering by a keyword is the catalog's: the tool belongs to a library.
  if (!directory) return null;
  const listMenu = (e: ReactMouseEvent<HTMLButtonElement>) => {
    e.preventDefault();
    const details = e.currentTarget.closest('details');
    if (details) details.open = true;
    const rect = e.currentTarget.getBoundingClientRect();
    showContextMenu(rect.left, rect.bottom, [
      { label: t('studio.library.keywordList.menu.new'), onClick: () => setMaking({ parent: null }) },
      ...(unused.length
        ? [
            {
              label: t('studio.library.keywordList.menu.deleteUnused'),
              onClick: () => setPending({ kind: 'deleteUnused', from: '', to: '', count: 0, unused })
            }
          ]
        : [])
    ]);
  };
  const makeKeys = (e: KeyboardEvent<HTMLInputElement>, parent: string | null) => {
    e.stopPropagation();
    if (e.key === 'Escape') return setMaking(null);
    if (e.key !== 'Enter') return;
    e.preventDefault();
    const text = e.currentTarget.value;
    setMaking(null);
    void make(parent, text);
  };
  return (
    <ToolGroup
      title="Keyword List"
      label={t('studio.library.keywordList.title')}
      actions={
        <button
          type="button"
          title={t('studio.library.keywordList.menu.new')}
          aria-label={t('studio.library.keywordList.menu.new')}
          onClick={listMenu}
        >
          <Plus size={12} />
        </button>
      }
    >
      {pending && (
        <div
          className="studio-inline studio-preset-confirm"
          role="alertdialog"
          aria-label={t('studio.library.keywordList.confirm.label')}
        >
          <span>{confirmText(pending)}</span>
          <span>
            <button onClick={() => setPending(null)}>{t('studio.library.keywordList.confirm.cancel')}</button>
            <button
              disabled={busy}
              onClick={() => {
                const change = pending;
                setPending(null);
                void run(change);
              }}
            >
              {actionLabel(pending.kind)}
            </button>
          </span>
        </div>
      )}
      {making && (
        <input
          className="studio-metadata-input studio-album-name"
          aria-label={
            making.parent
              ? t('studio.library.keywordList.new.insideLabel', { keyword: displayKeyword(making.parent) })
              : t('studio.library.keywordList.new.label')
          }
          autoFocus
          onKeyDown={(e) => makeKeys(e, making.parent)}
          onBlur={() => setMaking(null)}
        />
      )}
      {empty ? (
        <p className="studio-help">{t('studio.library.keywordList.empty')}</p>
      ) : (
        <div
          ref={list}
          className={`studio-presets studio-albums studio-keyword-list ${
            drag && drag.target === null && movedKeyword(drag.path, null) ? 'is-drop-end' : ''
          }`}
          role="tree"
          aria-label={t('studio.library.keywordList.title')}
          aria-busy={busy}
        >
          {rows(tree, 0)}
        </div>
      )}
    </ToolGroup>
  );
}
