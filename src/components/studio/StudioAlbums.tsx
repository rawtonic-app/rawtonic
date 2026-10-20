import { swallowDragClick } from '../../utils/swallowClick';
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { BookImage, ChevronRight, Folder, History, ListFilter, Plus, Unlink } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "react-toastify";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useContextMenu } from "../../context/ContextMenuContext";
import { Invokes } from "../ui/AppProperties";
import { ToolGroup } from "./StudioDevelop";
import { TILE_DROP_EVENT, type TileDropDetail } from "./tileDrag";
import { createDragGhost } from "./dragGhost";
import { MISSING_VIEW_ID, OFFLINE_CHANGED, OFFLINE_COUNTED, offlinePhotographs, openMissingView } from "../../utils/offline";
import { IMPORTS_CHANGED, PREVIOUS_IMPORT_VIEW_ID, openPreviousImport, previousImport } from "../../utils/folderSync";
import { CATALOG_EVENTS, SESSION_ALBUMS_CHANGED, catalogChanged, onCatalogChange } from "../../utils/catalogWrites";
import type { Group, Rule } from "../../utils/librarySearch";
import { asGroup, useSmartAlbumEditor } from "./SmartAlbumEditor";
import {
  changeLibraryAlbums,
  findAlbumItem,
  flattenAlbumTree,
  loadLibraryAlbums,
  withDeleted,
  withMoved,
  withNewItem,
  withPhotosAdded,
  withPhotosRemoved,
  withRenamed,
} from "./libraryAlbums";

export interface SessionAlbum {
  id: string;
  name: string;
  parent: string | null;
  count: number;
  /** "album" holds photographs, "group" holds albums and groups, "smart"
   * shows the photographs its rules match now. */
  kind?: "album" | "group" | "smart";
  position?: number;
  /** A smart album's rules. */
  rules?: Rule;
}

/** `activeAlbumId` of a session album shown in the grid. */
export const SESSION_ALBUM_PREFIX = "session-album:";
export { SESSION_ALBUMS_CHANGED } from "../../utils/catalogWrites";

export type SelectAlbum = (id: string, name: string, paths: string[], preserveEditor?: boolean) => void;

/** Shows a session album in the grid (also used to refresh it). */
export async function openSessionAlbum(
  directory: string,
  album: { id: string; name: string },
  select: SelectAlbum,
  preserveEditor = false,
) {
  const paths = await invoke<string[]>(Invokes.SessionAlbumPaths, { directory, id: album.id });
  select(`${SESSION_ALBUM_PREFIX}${album.id}`, album.name, paths, preserveEditor);
}

const isGroup = (album: SessionAlbum) => album.kind === "group";
const isSmart = (album: SessionAlbum) => album.kind === "smart";
/** A new smart album's first rule. */
const NEW_SMART_RULES: Group = { combine: "all", rules: [{ field: "rating", op: "ge", value: 3 }] };
/** How long catalog writes pause before smart albums are counted again. */
const SMART_REFRESH_MS = 250;

/** Albums of a parent in their order (the session returns them ordered). */
export function childrenOf(albums: SessionAlbum[], parent: string | null) {
  return albums.filter((album) => (album.parent ?? null) === parent);
}

/** Where on a row the pointer is: the edges place before/after it, the middle
 * of a group puts the dragged item into the group. */
export type DropZone = "before" | "after" | "into";

/** Parent and index for dropping `dragged` at `zone` of `target`, or null when
 * not allowed (onto itself, a group into its own groups). */
export function dropPlacement(
  albums: SessionAlbum[],
  draggedId: string,
  targetId: string | null,
  zone: DropZone,
): { parent: string | null; index: number } | null {
  if (targetId === draggedId) return null;
  const byId = new Map(albums.map((album) => [album.id, album]));
  const inside = (id: string | null) => {
    for (let at = id; at; at = byId.get(at)?.parent ?? null) if (at === draggedId) return true;
    return false;
  };
  if (targetId === null) {
    return { parent: null, index: childrenOf(albums, null).filter((a) => a.id !== draggedId).length };
  }
  const target = byId.get(targetId);
  if (!target) return null;
  if (zone === "into") {
    if (!isGroup(target) || inside(target.id)) return null;
    return { parent: target.id, index: childrenOf(albums, target.id).filter((a) => a.id !== draggedId).length };
  }
  const parent = target.parent ?? null;
  if (inside(parent)) return null;
  const siblings = childrenOf(albums, parent).filter((a) => a.id !== draggedId);
  const index = siblings.findIndex((a) => a.id === targetId);
  return { parent, index: index + (zone === "after" ? 1 : 0) };
}

type Naming = { kind: "album" | "group"; parent: string | null } | { rename: string };

/** What could not be done, as a toast (`studio.library.albums.toasts.*`). */
type Failure =
  | "openFailed"
  | "addFailed"
  | "removeFailed"
  | "renameFailed"
  | "createAlbumFailed"
  | "createGroupFailed"
  | "moveFailed"
  | "deleteFailed";

/** Where albums are kept: the session database in a session, the app-wide
 * album list (`albums.json`) outside sessions; the tool works the same on both. */
interface AlbumStore {
  add(id: string, paths: string[]): Promise<unknown>;
  remove(id: string, paths: string[]): Promise<unknown>;
  rename(id: string, name: string): Promise<unknown>;
  create(item: { kind: "album" | "group"; name: string; parent: string | null; paths: string[] }): Promise<unknown>;
  delete(id: string): Promise<unknown>;
  move(id: string, placement: { parent: string | null; index: number }): Promise<unknown>;
}

function sessionStore(directory: string): AlbumStore {
  return {
    add: (id, paths) => invoke(Invokes.SessionAlbumAdd, { directory, id, paths }),
    remove: (id, paths) => invoke(Invokes.SessionAlbumRemove, { directory, id, paths }),
    rename: (id, name) => invoke(Invokes.SessionRenameAlbum, { directory, id, name }),
    create: ({ kind, name, parent, paths }) =>
      invoke(Invokes.SessionCreateAlbum, { directory, name, paths: kind === "album" ? paths : [], kind, parent }),
    delete: (id) => invoke(Invokes.SessionDeleteAlbum, { directory, id }),
    move: (id, placement) => invoke(Invokes.SessionMoveAlbum, { directory, id, ...placement }),
  };
}

const libraryStore: AlbumStore = {
  add: (id, paths) => changeLibraryAlbums((tree) => withPhotosAdded(tree, id, paths)),
  remove: (id, paths) => changeLibraryAlbums((tree) => withPhotosRemoved(tree, id, paths)),
  rename: (id, name) => changeLibraryAlbums((tree) => withRenamed(tree, id, name)),
  create: (item) =>
    changeLibraryAlbums((tree) =>
      withNewItem(tree, { ...item, paths: item.kind === "album" ? item.paths : [], id: crypto.randomUUID() }),
    ),
  delete: (id) => changeLibraryAlbums((tree) => withDeleted(tree, id)),
  move: (id, { parent, index }) => changeLibraryAlbums((tree) => withMoved(tree, id, parent, index)),
};

/** Capture One's albums and groups / Lightroom's collections and sets: named
 * sets of photographs, made from the selection, nested in groups by dragging;
 * deleting one never touches its photographs. In a session they live in the
 * session database (they move with the session, in the order dragged);
 * outside sessions in the app's album list (in name order). */
export default function StudioAlbums({ onSelectAlbum }: { onSelectAlbum: SelectAlbum }) {
  const session = useLibraryStore((s) => s.sessionDirectory);
  const activeAlbumId = useLibraryStore((s) => s.activeAlbumId);
  const selected = useLibraryStore((s) => s.multiSelectedPaths);
  const albumTree = useLibraryStore((s) => s.albumTree);
  const { showContextMenu } = useContextMenu();
  const [sessionAlbums, setSessionAlbums] = useState<SessionAlbum[]>([]);
  const libraryAlbums = useMemo(() => flattenAlbumTree(albumTree ?? []), [albumTree]);
  const albums = session ? sessionAlbums : libraryAlbums;
  const store = useMemo(() => (session ? sessionStore(session) : libraryStore), [session]);
  /** `activeAlbumId` while the grid shows `album`. */
  const shownId = (album: SessionAlbum) => (session ? `${SESSION_ALBUM_PREFIX}${album.id}` : album.id);
  const [naming, setNaming] = useState<Naming | null>(null);
  const [deleting, setDeleting] = useState<SessionAlbum | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [drag, setDrag] = useState<{ id: string; target: string | null; zone: DropZone } | null>(null);
  const { t } = useTranslation();

  // Answers about a session closed meanwhile are dropped (a slow read must
  // not show the last session's albums or counts in the next one).
  const sessionRef = useRef(session);
  sessionRef.current = session;
  const stillShown = (asked: string | null) => sessionRef.current === asked;
  const reload = useCallback(async () => {
    try {
      if (session) {
        const list = (await invoke<SessionAlbum[]>(Invokes.SessionAlbums, { directory: session })) ?? [];
        if (stillShown(session)) setSessionAlbums(list);
      } else await loadLibraryAlbums();
    } catch (e) {
      toast.error(t("studio.library.albums.toasts.readFailed", { error: String(e) }));
    }
  }, [session, t]);
  useEffect(() => {
    void reload();
  }, [reload]);
  // A refreshed list (a rename, a move, background tagging: they change
  // the catalog without a catalog event) counts the smart albums again.
  const imageList = useLibraryStore((s) => s.imageList);
  useEffect(() => {
    if (!session) return;
    let alive = true;
    const timer = setTimeout(() => {
      invoke<SessionAlbum[]>(Invokes.SessionAlbums, { directory: session })
        .then((list) => {
          if (alive && stillShown(session) && list) setSessionAlbums(list);
        })
        .catch(() => {});
    }, SMART_REFRESH_MS);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [session, imageList]);
  // The photographs' context menu adds to and removes from albums too.
  const activeAlbumRef = useRef(activeAlbumId);
  activeAlbumRef.current = activeAlbumId;
  useEffect(() => {
    const changed = async () => {
      if (!session) return;
      const list = (await invoke<SessionAlbum[]>(Invokes.SessionAlbums, { directory: session }).catch(() => null)) ?? [];
      if (!stillShown(session)) return;
      setSessionAlbums(list);
      const active = activeAlbumRef.current;
      const album = active?.startsWith(SESSION_ALBUM_PREFIX)
        ? list.find((a) => a.id === active.slice(SESSION_ALBUM_PREFIX.length))
        : undefined;
      if (album) await openSessionAlbum(session, album, onSelectAlbum, true).catch(() => {});
    };
    const onChanged = () => void changed();
    window.addEventListener(SESSION_ALBUMS_CHANGED, onChanged);
    return () => window.removeEventListener(SESSION_ALBUMS_CHANGED, onChanged);
  }, [session, onSelectAlbum]);
  // A rating, label, flag or keyword written to the catalog, an import or a
  // relink: smart albums count again, and the one shown shows what its
  // rules match now (Lightroom's smart collections update live). The
  // callback is read when it is used: the parent passes a new one on every
  // render, which must not cancel a pending refresh.
  const selectRef = useRef(onSelectAlbum);
  selectRef.current = onSelectAlbum;
  useEffect(() => {
    if (!session) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // Only the newest refresh of this session may publish; navigating
    // away (another album, folder or session) meanwhile drops it.
    let generation = 0;
    let alive = true;
    const refresh = async () => {
      const mine = ++generation;
      const current = () => alive && mine === generation && stillShown(session);
      const list = (await invoke<SessionAlbum[]>(Invokes.SessionAlbums, { directory: session }).catch(() => null)) ?? [];
      if (!current()) return;
      setSessionAlbums(list);
      const active = activeAlbumRef.current;
      const album = active?.startsWith(SESSION_ALBUM_PREFIX)
        ? list.find((a) => a.id === active.slice(SESSION_ALBUM_PREFIX.length))
        : undefined;
      if (!album || !isSmart(album)) return;
      const select: SelectAlbum = (...args) => {
        if (current() && activeAlbumRef.current === active) selectRef.current(...args);
      };
      await openSessionAlbum(session, album, select, true).catch(() => {});
    };
    const changed = () => {
      clearTimeout(timer);
      timer = setTimeout(() => void refresh(), SMART_REFRESH_MS);
    };
    // Album membership has its own refresh here (SESSION_ALBUMS_CHANGED).
    const stop = onCatalogChange(
      changed,
      CATALOG_EVENTS.filter((event) => event !== SESSION_ALBUMS_CHANGED),
    );
    return () => {
      alive = false;
      clearTimeout(timer);
      stop();
    };
  }, [session]);

  /** Offline photographs of the session (Missing Photographs, shown when any). */
  const [missing, setMissing] = useState(0);
  const recountMissing = useCallback(async () => {
    if (!session) return setMissing(0);
    const count = await offlinePhotographs(session).then((list) => (list ?? []).length).catch(() => 0);
    if (stillShown(session)) setMissing(count);
  }, [session]);
  useEffect(() => {
    void recountMissing();
  }, [recountMissing]);
  const showMissing = useCallback(
    (preserveEditor = false) => {
      if (!session) return Promise.resolve(0);
      return openMissingView(session, t("offline.missingView"), onSelectAlbum, preserveEditor).then((count) => {
        if (stillShown(session)) setMissing(count);
        return count;
      });
    },
    [session, t, onSelectAlbum],
  );
  // After a relink: a new count; the Missing Photographs view shows what is left.
  useEffect(() => {
    const changed = () => {
      if (activeAlbumRef.current === MISSING_VIEW_ID) void showMissing(true).catch(() => {});
      else void recountMissing();
    };
    // Another view's check found a different set: the count follows.
    const counted = (event: Event) => {
      const entries = (event as CustomEvent<unknown[]>).detail;
      if (Array.isArray(entries)) setMissing(entries.length);
    };
    window.addEventListener(OFFLINE_CHANGED, changed);
    window.addEventListener(OFFLINE_COUNTED, counted);
    return () => {
      window.removeEventListener(OFFLINE_CHANGED, changed);
      window.removeEventListener(OFFLINE_COUNTED, counted);
    };
  }, [showMissing, recountMissing]);

  /** Photographs of the newest import batch (Previous Import, shown when any). */
  const [imported, setImported] = useState(0);
  const recountImported = useCallback(async () => {
    if (!session) return setImported(0);
    const count = await previousImport(session).then((batch) => batch.paths.length).catch(() => 0);
    if (stillShown(session)) setImported(count);
  }, [session]);
  useEffect(() => {
    void recountImported();
  }, [recountImported]);
  const showImported = useCallback(
    (preserveEditor = false) => {
      if (!session) return Promise.resolve(0);
      return openPreviousImport(session, t("studio.library.albums.previousImport"), onSelectAlbum, preserveEditor).then(
        (count) => {
          if (stillShown(session)) setImported(count);
          return count;
        },
      );
    },
    [session, t, onSelectAlbum],
  );
  // An ingest or a synchronization added photographs: a new batch.
  useEffect(() => {
    const changed = () => {
      if (activeAlbumRef.current === PREVIOUS_IMPORT_VIEW_ID) void showImported(true).catch(() => {});
      else void recountImported();
    };
    window.addEventListener(IMPORTS_CHANGED, changed);
    return () => window.removeEventListener(IMPORTS_CHANGED, changed);
  }, [showImported, recountImported]);

  /** Shows `album` in the grid (`preserveEditor` refreshes it in place). */
  const show = async (album: SessionAlbum, preserveEditor = false) => {
    if (session) return openSessionAlbum(session, album, onSelectAlbum, preserveEditor);
    const item = findAlbumItem(useLibraryStore.getState().albumTree, album.id);
    if (item?.type === "album") onSelectAlbum(item.id, item.name, item.images, preserveEditor);
  };
  const run = async (work: () => Promise<unknown>, failure: Failure, album?: SessionAlbum) => {
    try {
      await work();
      // The app-wide albums are reloaded by the change itself; in a session,
      // searches, lists and smart albums (album rules) follow it too.
      if (session) {
        catalogChanged();
        await reload();
      }
      // The grid shows this album: show its new content.
      if (album && activeAlbumId === shownId(album)) await show(album, true);
    } catch (e) {
      toast.error(t(`studio.library.albums.toasts.${failure}`, { error: String(e) }));
    }
  };
  const open = (album: SessionAlbum) => run(() => show(album), "openFailed");
  const startNew = (kind: "album" | "group", parent: string | null) => {
    if (parent) setCollapsed((previous) => new Set([...previous].filter((id) => id !== parent)));
    setNaming({ kind, parent });
  };
  const openEditor = useSmartAlbumEditor((s) => s.open);
  const newSmart = (parent: string | null) =>
    openEditor({ name: t("studio.library.smartAlbums.defaultName"), parent, rules: NEW_SMART_RULES });
  const newMenu = (x: number, y: number, parent: string | null) =>
    showContextMenu(x, y, [
      {
        label: selected.length
          ? t("studio.library.albums.menu.newAlbumFromSelected", { count: selected.length })
          : t("studio.library.albums.menu.newAlbum"),
        onClick: () => startNew("album", parent),
      },
      ...(session ? [{ label: t("studio.library.albums.menu.newSmartAlbum"), onClick: () => newSmart(parent) }] : []),
      { label: t("studio.library.albums.menu.newGroup"), onClick: () => startNew("group", parent) },
    ]);
  const menu = (e: MouseEvent, album: SessionAlbum) => {
    e.preventDefault();
    if (isGroup(album)) {
      showContextMenu(e.clientX, e.clientY, [
        { label: t("studio.library.albums.menu.newAlbumInGroup"), onClick: () => startNew("album", album.id) },
        ...(session
          ? [{ label: t("studio.library.albums.menu.newSmartAlbumInGroup"), onClick: () => newSmart(album.id) }]
          : []),
        { label: t("studio.library.albums.menu.newGroupInGroup"), onClick: () => startNew("group", album.id) },
        { label: t("studio.library.albums.menu.rename"), onClick: () => setNaming({ rename: album.id }) },
        { label: t("studio.library.albums.menu.deleteGroup"), onClick: () => setDeleting(album) },
      ]);
      return;
    }
    if (isSmart(album)) {
      showContextMenu(e.clientX, e.clientY, [
        {
          label: t("studio.library.albums.menu.editSmartAlbum"),
          onClick: () =>
            openEditor({
              id: album.id,
              name: album.name,
              parent: album.parent,
              rules: asGroup(album.rules ?? NEW_SMART_RULES),
            }),
        },
        { label: t("studio.library.albums.menu.rename"), onClick: () => setNaming({ rename: album.id }) },
        { label: t("studio.library.albums.menu.deleteSmartAlbum"), onClick: () => setDeleting(album) },
      ]);
      return;
    }
    const count = selected.length;
    showContextMenu(e.clientX, e.clientY, [
      {
        label: count
          ? t("studio.library.albums.menu.addSelectedCount", { count })
          : t("studio.library.albums.menu.addSelected"),
        disabled: !count,
        onClick: () => run(() => store.add(album.id, selected), "addFailed", album),
      },
      {
        label: t("studio.library.albums.menu.removeSelected"),
        disabled: !count || activeAlbumId !== shownId(album),
        onClick: () => run(() => store.remove(album.id, selected), "removeFailed", album),
      },
      { label: t("studio.library.albums.menu.rename"), onClick: () => setNaming({ rename: album.id }) },
      { label: t("studio.library.albums.menu.deleteAlbum"), onClick: () => setDeleting(album) },
    ]);
  };
  const nameKeys = (e: KeyboardEvent<HTMLInputElement>) => {
    e.stopPropagation();
    if (e.key === "Escape") return setNaming(null);
    if (e.key !== "Enter" || !naming) return;
    const name = e.currentTarget.value.trim();
    const current = naming;
    setNaming(null);
    if (!name) return;
    if ("rename" in current) {
      const album = albums.find((a) => a.id === current.rename);
      if (album && name !== album.name)
        void run(() => store.rename(album.id, name), "renameFailed");
    } else {
      void run(
        () => store.create({ kind: current.kind, name, parent: current.parent, paths: selected }),
        current.kind === "group" ? "createGroupFailed" : "createAlbumFailed",
      );
    }
  };

  // Photographs dragged from the grid onto an album (tileDrag.ts).
  const list = useRef<HTMLDivElement>(null);
  const dropRef = useRef<(detail: TileDropDetail) => void>(() => {});
  dropRef.current = ({ target, paths }) => {
    const album = albums.find((a) => a.id === target);
    if (!album || isGroup(album) || isSmart(album) || !paths.length) return;
    void run(() => store.add(album.id, paths), "addFailed", album);
  };
  useEffect(() => {
    const element = list.current;
    if (!element) return;
    const onDrop = (e: Event) => dropRef.current((e as CustomEvent<TileDropDetail>).detail);
    element.addEventListener(TILE_DROP_EVENT, onDrop);
    return () => element.removeEventListener(TILE_DROP_EVENT, onDrop);
  }, [session]);

  // Albums and groups reordered or moved into groups by dragging (pointer
  // events, as for photographs).
  const dragRef = useRef<{ id: string; x: number; y: number; moved: boolean } | null>(null);
  const zoneAt = (x: number, y: number): { target: string | null; zone: DropZone } | null => {
    const row = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-album-row]");
    if (!row) return list.current?.contains(document.elementFromPoint(x, y)) ? { target: null, zone: "after" } : null;
    const rect = row.getBoundingClientRect();
    const at = rect.height ? (y - rect.top) / rect.height : 0.5;
    const group = row.dataset.kind === "group";
    return {
      target: row.dataset.albumRow ?? null,
      zone: group && at > 0.25 && at < 0.75 ? "into" : at < 0.5 ? "before" : "after",
    };
  };
  const pressRow = (e: ReactPointerEvent<HTMLElement>, album: SessionAlbum) => {
    if (e.button !== 0) return;
    dragRef.current = { id: album.id, x: e.clientX, y: e.clientY, moved: false };
    let ghost: ReturnType<typeof createDragGhost> | null = null;
    const move = (ev: PointerEvent) => {
      const state = dragRef.current;
      if (!state) return;
      if (!state.moved && Math.hypot(ev.clientX - state.x, ev.clientY - state.y) < 5) return;
      state.moved = true;
      ghost ??= createDragGhost({ label: album.name, kind: album.kind === "group" ? "group" : "album" });
      ghost.move(ev.clientX, ev.clientY);
      const at = zoneAt(ev.clientX, ev.clientY);
      setDrag(at ? { id: state.id, ...at } : { id: state.id, target: null, zone: "before" });
    };
    const end = (ev: PointerEvent) => {
      document.removeEventListener("pointermove", move, true);
      document.removeEventListener("pointerup", end, true);
      document.removeEventListener("pointercancel", end, true);
      const state = dragRef.current;
      dragRef.current = null;
      setDrag(null);
      ghost?.remove();
      if (!state?.moved || ev.type !== "pointerup") return;
      // The press ended a drag, not a click on the row.
      swallowDragClick(false);
      const at = zoneAt(ev.clientX, ev.clientY);
      const placement = at && dropPlacement(albums, state.id, at.target, at.zone);
      if (!placement) return;
      void run(() => store.move(state.id, placement), "moveFailed");
    };
    document.addEventListener("pointermove", move, true);
    document.addEventListener("pointerup", end, true);
    document.addEventListener("pointercancel", end, true);
  };

  const nameInput = (parentDepth: number, label: string, defaultValue = "") => (
    <input
      key={`naming-${label}`}
      className="studio-metadata-input studio-album-name"
      style={{ marginLeft: parentDepth * 16 }}
      aria-label={label}
      placeholder={label}
      defaultValue={defaultValue}
      autoFocus
      onKeyDown={nameKeys}
      onBlur={() => setNaming(null)}
    />
  );
  const rows = (parent: string | null, depth: number): ReactNode[] =>
    childrenOf(albums, parent).flatMap((album) => {
      const group = isGroup(album);
      const expanded = !collapsed.has(album.id);
      const dropClass =
        drag && drag.target === album.id && drag.id !== album.id
          ? dropPlacement(albums, drag.id, album.id, drag.zone)
            ? ` is-drop-${drag.zone}`
            : ""
          : "";
      const row =
        naming && "rename" in naming && naming.rename === album.id ? (
          nameInput(depth, t("studio.library.albums.renameLabel", { name: album.name }), album.name)
        ) : (
          <button
            key={album.id}
            role="listitem"
            data-album-row={album.id}
            data-kind={group ? "group" : "album"}
            {...(group || isSmart(album)
              ? {}
              : { "data-album-drop": album.id, "data-album-name": album.name })}
            {...(isSmart(album) ? { "data-drop-refused": t("studio.library.albums.smartNoDrop") } : {})}
            title={
              group
                ? t("studio.library.albums.groupHint")
                : isSmart(album)
                  ? t("studio.library.albums.smartHint")
                  : t("studio.library.albums.albumHint")
            }
            aria-expanded={group ? expanded : undefined}
            className={`studio-preset studio-album ${group ? "is-group" : ""} ${
              activeAlbumId === shownId(album) ? "is-applied" : ""
            } ${drag?.id === album.id ? "is-dragged" : ""}${dropClass}`}
            // Albums leave room for a group's chevron, so icons line up.
            style={{ paddingLeft: 6 + depth * 16 + (group ? 0 : 16) }}
            onPointerDown={(e) => pressRow(e, album)}
            onClick={() =>
              group
                ? setCollapsed((previous) => {
                    const next = new Set(previous);
                    if (next.has(album.id)) next.delete(album.id);
                    else next.add(album.id);
                    return next;
                  })
                : void open(album)
            }
            onContextMenu={(e) => menu(e, album)}
          >
            {group ? (
              <>
                <ChevronRight size={11} className={expanded ? "is-open" : ""} />
                <Folder size={12} />
              </>
            ) : isSmart(album) ? (
              <ListFilter size={12} />
            ) : (
              <BookImage size={12} />
            )}
            <span>{album.name}</span>
            {!group && <small>{album.count}</small>}
          </button>
        );
      const inner =
        group && expanded
          ? [
              ...rows(album.id, depth + 1),
              ...(naming && !("rename" in naming) && naming.parent === album.id
                ? [
                    nameInput(
                      depth + 1,
                      naming.kind === "group"
                        ? t("studio.library.albums.newGroupName")
                        : t("studio.library.albums.newAlbumName"),
                    ),
                  ]
                : []),
            ]
          : [];
      return [row, ...inner];
    });

  return (
    <ToolGroup
      title="Albums"
      label={t("studio.library.albums.title")}
      actions={
        <button
          type="button"
          title={t("studio.library.albums.newTitle")}
          aria-label={t("studio.library.albums.menu.newAlbum")}
          onClick={(e) => {
            e.preventDefault();
            const details = e.currentTarget.closest("details");
            if (details) details.open = true;
            const rect = e.currentTarget.getBoundingClientRect();
            newMenu(rect.left, rect.bottom, null);
          }}
        >
          <Plus size={12} />
        </button>
      }
    >
      {deleting && (
        <div className="studio-inline studio-preset-confirm" role="alertdialog" aria-label={t("studio.library.albums.confirm.label")}>
          <span>
            {isGroup(deleting)
              ? t("studio.library.albums.confirm.deleteGroup", { name: deleting.name })
              : isSmart(deleting)
                ? t("studio.library.albums.confirm.deleteSmartAlbum", { name: deleting.name })
                : t("studio.library.albums.confirm.deleteAlbum", { name: deleting.name })}
          </span>
          <span>
            <button onClick={() => setDeleting(null)}>{t("studio.library.albums.confirm.cancel")}</button>
            <button
              onClick={() => {
                const album = deleting;
                setDeleting(null);
                void run(() => store.delete(album.id), "deleteFailed");
              }}
            >
              {t("studio.library.albums.confirm.delete")}
            </button>
          </span>
        </div>
      )}
      {albums.length === 0 && !naming && (
        <p className="studio-help">{t("studio.library.albums.empty")}</p>
      )}
      <div
        ref={list}
        className={`studio-presets studio-albums ${drag && drag.target === null ? "is-drop-end" : ""}`}
        role="list"
        aria-label={t("studio.library.albums.title")}
      >
        {session && imported > 0 && (
          // Lightroom's Previous Import: the newest ingest, Add or Synchronize Folder.
          <button
            key={PREVIOUS_IMPORT_VIEW_ID}
            role="listitem"
            className={`studio-preset studio-album is-smart ${activeAlbumId === PREVIOUS_IMPORT_VIEW_ID ? "is-applied" : ""}`}
            style={{ paddingLeft: 22 }}
            title={t("studio.library.albums.previousImportTitle")}
            data-drop-refused={t("studio.library.albums.previousImportNoDrop")}
            onClick={() => void showImported().catch((e) => toast.error(String(e)))}
          >
            <History size={12} />
            <span>{t("studio.library.albums.previousImport")}</span>
            <small>{imported}</small>
          </button>
        )}
        {session && missing > 0 && (
          // Lightroom's Missing Photographs collection: only while there are any.
          <button
            key={MISSING_VIEW_ID}
            role="listitem"
            className={`studio-preset studio-album is-smart ${activeAlbumId === MISSING_VIEW_ID ? "is-applied" : ""}`}
            style={{ paddingLeft: 22 }}
            title={t("offline.missingViewTitle")}
            data-drop-refused={t("offline.missingViewNoDrop")}
            onClick={() => void showMissing().catch((e) => toast.error(String(e)))}
          >
            <Unlink size={12} />
            <span>{t("offline.missingView")}</span>
            <small>{missing}</small>
          </button>
        )}
        {rows(null, 0)}
        {naming &&
          !("rename" in naming) &&
          naming.parent === null &&
          nameInput(
            0,
            naming.kind === "group"
              ? t("studio.library.albums.newGroupName")
              : t("studio.library.albums.newAlbumName"),
          )}
      </div>
    </ToolGroup>
  );
}
