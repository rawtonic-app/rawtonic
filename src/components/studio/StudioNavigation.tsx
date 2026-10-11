import { Fragment, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowDownAZ,
  Check,
  ChevronRight,
  ClipboardPaste,
  FileEdit,
  Folder,
  FolderInput,
  FolderMinus,
  FolderOpen,
  FolderPlus,
  FolderSearch,
  FolderX,
  HardDrive,
  RefreshCw,
  Star,
  StarOff,
  Trash2,
} from "lucide-react";
import { toast } from "react-toastify";
import {
  OFFLINE_CHANGED,
  OFFLINE_COUNTED,
  offlineBelow,
  offlineFolders,
  offlinePhotographs,
  type OfflineFolder,
} from "../../utils/offline";
import { useContextMenu } from "../../context/ContextMenuContext";
import {
  Invokes,
  OPTION_SEPARATOR,
  SortDirection,
  type FolderTreeSort,
  type OfflineEntry,
  type Option,
} from "../ui/AppProperties";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { useProcessStore } from "../../store/useProcessStore";
import { useLibraryActions } from "../../hooks/useLibraryActions";
import { ToolGroup } from "./StudioDevelop";
import { ToolColumn } from "./toolLayout";
import { StudioExif, StudioKeywords, StudioMetadataTool } from "./StudioMetadata";
import StudioAlbums, { type SelectAlbum } from "./StudioAlbums";
import {
  DEFAULT_FOLDER_TREE_SORT,
  filterFolderTree,
  sortFolderTree,
  toggleFolder,
  type FolderTree,
} from "../../utils/folderTree";
import { isInternalSessionFolder } from "../../utils/sessionFolders";
import {
  catalogTreePaths,
  formatSpace,
  isBelow,
  missingFolders,
  volumeSections,
  withMissingFolders,
  type CatalogFoldersView,
  type VolumeFolders,
} from "../../utils/catalogFolders";

/** File operations on folders: journaled and undoable where they move or
 * trash (useFileOperations); the library list itself never touches disk. */
export interface FolderActions {
  moveHere(paths: string[], folder: string): void;
  trash(folder: string): void;
  /** Adds a folder to the library (a folder dialog). */
  addFolder?(): void;
  /** Takes a top-level folder off the library list; nothing on disk changes. */
  removeFromLibrary?(folder: string): void;
  /** Pastes the photographs copied with ⌘C into the folder. */
  paste?(mode: "copy" | "move", folder: string): void;
  /** Asks where a folder gone from disk is now and relinks its photographs
   * (Lightroom's Find Missing Folder). */
  locateFolder?(folder: string): void;
}

/** The session's own folders (Capture One's Capture, Selects, Output, Trash). */
const SESSION_FOLDERS = new Set(["Capture", "Selects", "Output", "Trash"]);

/** Typed text must be this long before it filters (one letter matches nearly everything). */
const MIN_FILTER = 2;

const SORT_KEYS: FolderTreeSort["key"][] = ["name", "created", "modified", "imageCount"];

/** The trees filtered by folder name (when `filter` is set) and sorted. */
function shapeTrees(list: FolderTree[], filter: string, sort: FolderTreeSort) {
  const kept = filter
    ? list.map((tree) => filterFolderTree(tree, filter)).filter((tree): tree is FolderTree => tree !== null)
    : list;
  return sortFolderTree(kept, sort);
}

interface RowContext {
  current: string | null;
  onNavigate(path: string): void;
  sessionRoot: string | null;
  actions?: FolderActions;
  /** Every folder is shown open (while a filter is typed). */
  allOpen: boolean;
  showCounts: boolean;
  /** Folders holding offline photographs (Lightroom marks them). */
  offline: OfflineFolder[];
  /** The tooltip of a folder gone from disk. */
  missingTitle(path: string): string;
}

function FolderRow({
  node,
  depth = 0,
  parent,
  context,
}: {
  node: FolderTree;
  depth?: number;
  parent?: string;
  context: RowContext;
}) {
  const { current, onNavigate, sessionRoot, actions, allOpen, showCounts, offline, missingTitle } = context;
  const { t } = useTranslation();
  const missingHere = offlineBelow(offline, node.path);
  // Gone from disk: known from the library, listed at its place.
  const missing = !!node.missing;
  const { showContextMenu } = useContextMenu();
  const { handleTogglePinFolder, refreshAllFolderTrees } = useLibraryActions();
  const isMac = useSettingsStore((s) => s.osPlatform === "macos");
  const pinned = useSettingsStore((s) => s.appSettings?.pinnedFolders?.includes(node.path) ?? false);
  const expanded = useLibraryStore((s) => s.expandedFolders.has(node.path));
  const path = node.path;
  const children = (node.children ?? []).filter(
    (child) => !isInternalSessionFolder(path, child.name, sessionRoot),
  );
  const open = allOpen || expanded;
  const canExpand = children.length > 0 || !!node.hasSubdirs;
  const menu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (missing) {
      // Nothing on disk to move, rename or show; it can be located.
      showContextMenu(e.clientX, e.clientY, [
        ...(actions?.locateFolder
          ? [{ label: t("offline.menu.locateFolder"), icon: FolderSearch, onClick: () => actions.locateFolder?.(path) }]
          : []),
        {
          label: t("studio.library.folders.menu.refresh"),
          icon: RefreshCw,
          onClick: () => void refreshAllFolderTrees(),
        },
      ]);
      return;
    }
    const { multiSelectedPaths, rootPaths } = useLibraryStore.getState();
    const { copiedFilePaths } = useProcessStore.getState();
    const parentOf = (p: string) => p.slice(0, Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\")));
    const toMove = multiSelectedPaths.filter((p) => parentOf(p.split("?vc=")[0]) !== path);
    const isRoot = rootPaths.includes(path);
    // The session itself, its standard folders and library roots stay.
    const fixed =
      path === sessionRoot ||
      isRoot ||
      (!!sessionRoot && parent === sessionRoot && SESSION_FOLDERS.has(node.name));
    const copied = new Set(copiedFilePaths.map((p) => p.split("?vc=")[0])).size;
    const { setUI } = useUIStore.getState();
    const fileItems: Option[] = actions
      ? [
          ...(toMove.length
            ? [
                {
                  label: t("studio.library.folders.menu.moveSelectedHere", { count: toMove.length }),
                  icon: FolderInput,
                  onClick: () => actions.moveHere(toMove, path),
                },
              ]
            : []),
          // Photographs copied with ⌘C: copied (copy_files refuses names that
          // exist there) or moved (journaled, undoable) into this folder.
          ...(actions.paste && copied
            ? [
                {
                  label: t("studio.library.folders.menu.pasteCopiesHere", { count: copied }),
                  icon: ClipboardPaste,
                  onClick: () => actions.paste?.("copy", path),
                },
                {
                  label: t("studio.library.folders.menu.moveCopiedHere", { count: copied }),
                  icon: FolderInput,
                  onClick: () => actions.paste?.("move", path),
                },
              ]
            : []),
          {
            label: t("studio.library.folders.menu.newFolder"),
            icon: FolderPlus,
            onClick: () => setUI({ folderActionTarget: path, isCreateFolderModalOpen: true }),
          },
          {
            label: t("studio.library.folders.menu.renameFolder"),
            icon: FileEdit,
            disabled: fixed,
            onClick: () => setUI({ folderActionTarget: path, isRenameFolderModalOpen: true }),
          },
          {
            label: t("studio.library.folders.menu.trashFolder"),
            icon: Trash2,
            disabled: fixed,
            isDestructive: true,
            onClick: () => actions.trash(path),
          },
          { type: OPTION_SEPARATOR },
        ]
      : [];
    showContextMenu(e.clientX, e.clientY, [
      ...fileItems,
      {
        label: pinned
          ? t("studio.library.folders.menu.removeFromFavorites")
          : t("studio.library.folders.menu.addToFavorites"),
        icon: pinned ? StarOff : Star,
        onClick: () => void handleTogglePinFolder(path),
      },
      ...(actions?.removeFromLibrary && isRoot && path !== sessionRoot
        ? [
            {
              // Only the list changes: the folder and its photographs stay on disk.
              label: t("studio.library.folders.menu.removeFromLibrary"),
              icon: FolderMinus,
              onClick: () => actions.removeFromLibrary?.(path),
            },
          ]
        : []),
      { type: OPTION_SEPARATOR },
      {
        label: isMac
          ? t("studio.library.folders.menu.showInFinder")
          : t("studio.library.folders.menu.showInFileManager"),
        icon: FolderSearch,
        onClick: () =>
          void invoke(Invokes.ShowInFinder, { path }).catch((err) =>
            toast.error(t("studio.library.folders.toasts.showFailed", { error: String(err) })),
          ),
      },
      {
        label: t("studio.library.folders.menu.refresh"),
        icon: RefreshCw,
        onClick: () => void refreshAllFolderTrees(),
      },
    ]);
  };
  return (
    <>
      <div
        className={`studio-folder ${current === path ? "is-selected" : ""} ${missing ? "is-offline" : ""}`}
        style={{ paddingLeft: 10 + depth * 12 }}
        onContextMenu={menu}
      >
        <button
          className={open && canExpand ? "is-expanded" : ""}
          aria-label={
            open
              ? t("studio.library.folders.collapse", { name: node.name })
              : t("studio.library.folders.expand", { name: node.name })
          }
          aria-expanded={canExpand ? open : undefined}
          disabled={!canExpand || allOpen}
          onClick={() => void toggleFolder(path, !missing)}
        >
          <ChevronRight size={11} />
        </button>
        <button onClick={() => onNavigate(path)} title={missing ? missingTitle(path) : path}>
          {missing ? <FolderX size={14} /> : current === path ? <FolderOpen size={14} /> : <Folder size={14} />}
          <span>{node.label || node.name || path.split(/[\\/]/).pop()}</span>
          {showCounts && (node.imageCount ?? 0) > 0 && <small>{node.imageCount}</small>}
          {missingHere > 0 && (
            <small className="studio-folder-offline" title={t("offline.folders.offlineInside", { count: missingHere })}>
              {`?${missingHere}`}
            </small>
          )}
        </button>
      </div>
      {open &&
        children.map((child) => (
          <FolderRow key={child.path} node={child} depth={depth + 1} parent={path} context={context} />
        ))}
    </>
  );
}

/** A drive in a catalog's Folders panel (Lightroom's volume bar): its name,
 * whether it is connected and its free space; a click folds its folders. */
function VolumeHeader({
  volume,
  collapsed,
  onToggle,
}: {
  volume: VolumeFolders;
  collapsed: boolean;
  onToggle(): void;
}) {
  const { t } = useTranslation();
  const space = formatSpace(volume.free_bytes, volume.total_bytes);
  const low =
    volume.online && volume.free_bytes !== null && !!volume.total_bytes && volume.free_bytes / volume.total_bytes < 0.1;
  const title = volume.online
    ? space
      ? t("offline.folders.volumeSpaceTitle", { name: volume.name, mount: volume.mount_point, space })
      : t("offline.folders.volumeTitle", { name: volume.name, mount: volume.mount_point })
    : t("offline.folders.volumeOfflineTitle", { name: volume.name });
  return (
    <div
      className={`studio-folder studio-volume ${volume.online ? "" : "is-offline"}`}
      data-volume={volume.key}
      data-online={volume.online}
    >
      <button
        className={collapsed ? "" : "is-expanded"}
        aria-label={
          collapsed
            ? t("studio.library.folders.expand", { name: volume.name })
            : t("studio.library.folders.collapse", { name: volume.name })
        }
        aria-expanded={!collapsed}
        onClick={onToggle}
      >
        <ChevronRight size={11} />
      </button>
      <button onClick={onToggle} title={title}>
        <HardDrive size={14} />
        <span>{volume.name}</span>
        <i
          className={`studio-volume-state ${volume.online ? (low ? "is-low" : "is-online") : ""}`}
          aria-hidden
        />
        <small className={volume.online ? "" : "studio-folder-offline"}>
          {volume.online ? space ?? "" : t("offline.folders.volumeOffline")}
        </small>
      </button>
    </div>
  );
}

/** Lightroom's Folders panel / Capture One's Library folders: the library's
 * folders (a session's own in a session) with favourites on top, filtered by
 * name and sorted; folders are added to and removed from the list without
 * touching disk, and file operations on them are journaled. A catalog lists
 * its folders under the drive they are on, connected or not, including the
 * folders its photographs were followed or relinked to; folders gone from
 * disk stay at their place with a missing marker (utils/catalogFolders.ts). */
export function StudioFolders({
  onNavigate,
  actions,
}: {
  onNavigate(path: string): void;
  actions?: FolderActions;
}) {
  const trees = useLibraryStore((s) => s.folderTrees);
  const pinnedTrees = useLibraryStore((s) => s.pinnedFolderTrees);
  const catalogView = useLibraryStore((s) => s.catalogFolders);
  const catalogTrees = useLibraryStore((s) => s.catalogTrees);
  const rootPaths = useLibraryStore((s) => s.rootPaths);
  const sessionRoot = useLibraryStore((s) => s.sessionDirectory);
  const current = useLibraryStore((s) => s.currentFolderPath);
  const loading = useLibraryStore((s) => s.isTreeLoading);
  const countsWanted = useSettingsStore((s) => s.appSettings?.enableFolderImageCounts ?? false);
  const sort = useSettingsStore((s) => s.appSettings?.folderTreeSort) ?? DEFAULT_FOLDER_TREE_SORT;
  const { showContextMenu } = useContextMenu();
  const [query, setQuery] = useState("");
  const [collapsedVolumes, setCollapsedVolumes] = useState<Set<string>>(() => new Set());
  const filter = query.trim();
  const filtering = filter.length >= MIN_FILTER;
  const { t } = useTranslation();
  // Offline photographs of the session or catalog, by folder: existing
  // folders get a marker, folders gone from disk (renamed, moved, drive
  // disconnected) are listed at their place (Lightroom's "?" folders). A
  // catalog's folders come by drive.
  const [offline, setOffline] = useState<OfflineFolder[]>([]);
  const rootsKey = rootPaths.join("\n");
  useEffect(() => {
    const { setLibrary } = useLibraryStore.getState();
    if (!sessionRoot) {
      setOffline([]);
      setLibrary({ catalogFolders: null });
      return;
    }
    let live = true;
    const loadFolders = () =>
      invoke<CatalogFoldersView>(Invokes.CatalogFolders, {
        directory: sessionRoot,
        roots: rootsKey ? rootsKey.split("\n") : [],
      })
        .then((view) => {
          if (live) setLibrary({ catalogFolders: view ?? null });
        })
        .catch(() => {
          if (live) setLibrary({ catalogFolders: null });
        });
    // The folders after the check: it updates which photographs are missing.
    const load = () =>
      void offlinePhotographs(sessionRoot)
        .then((entries) => {
          if (live) setOffline(offlineFolders(entries ?? []));
        })
        .catch(() => {
          if (live) setOffline([]);
        })
        .finally(() => void loadFolders());
    // Another view's check found a different set (a drive followed).
    const counted = (event: Event) => {
      const entries = (event as CustomEvent<OfflineEntry[]>).detail;
      if (live && Array.isArray(entries)) setOffline(offlineFolders(entries));
      void loadFolders();
    };
    load();
    window.addEventListener(OFFLINE_CHANGED, load);
    window.addEventListener(OFFLINE_COUNTED, counted);
    return () => {
      live = false;
      window.removeEventListener(OFFLINE_CHANGED, load);
      window.removeEventListener(OFFLINE_COUNTED, counted);
    };
  }, [sessionRoot, trees, rootsKey]);

  // Disk trees of top-level catalog folders that are not library roots
  // (photographs found, followed or relinked outside the added folders).
  const catalogPaths = useMemo(() => catalogTreePaths(catalogView, rootPaths), [catalogView, rootPaths]);
  const catalogKey = catalogPaths.join("\n");
  useEffect(() => {
    const { setLibrary } = useLibraryStore.getState();
    if (!catalogKey) {
      if (useLibraryStore.getState().catalogTrees.length) setLibrary({ catalogTrees: [] });
      return;
    }
    let live = true;
    invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
      paths: catalogKey.split("\n"),
      expandedFolders: [...useLibraryStore.getState().expandedFolders],
      // Catalog folders are counted, as the library's are.
      showImageCounts: true,
    })
      .then((loaded) => {
        if (live) setLibrary({ catalogTrees: loaded ?? [] });
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [catalogKey]);

  const catalog = !!catalogView?.catalog;
  const missing = useMemo(() => missingFolders(catalogView, offline), [catalogView, offline]);
  const sections = useMemo(
    () => volumeSections(catalogView, [...trees, ...catalogTrees], offline),
    [catalogView, trees, catalogTrees, offline],
  );
  // A session's (or a plain folder list's) trees with its missing folders
  // at their place.
  const libraryTrees = useMemo(() => withMissingFolders(trees, missing), [trees, missing]);
  const listed = useMemo(() => {
    const paths = new Set<string>();
    const walk = (list: FolderTree[]) =>
      list.forEach((node) => {
        paths.add(node.path);
        walk(node.children ?? []);
      });
    if (catalog) sections.forEach((section) => walk(section.trees));
    else walk(libraryTrees);
    walk(pinnedTrees);
    return paths;
  }, [catalog, sections, libraryTrees, pinnedTrees]);
  // Offline photographs' folders outside every listed folder.
  const unlisted = offline.filter(
    (folder) => !listed.has(folder.path) && ![...listed].some((path) => isBelow(folder.path, path)),
  );

  const shownTrees = useMemo(
    () => shapeTrees(libraryTrees, filtering ? filter : "", sort),
    [libraryTrees, filter, filtering, sort],
  );
  const shownPinned = useMemo(
    () => shapeTrees(pinnedTrees, filtering ? filter : "", sort),
    [pinnedTrees, filter, filtering, sort],
  );
  const shownSections = useMemo(
    () =>
      sections
        .map((section) => ({ ...section, trees: shapeTrees(section.trees, filtering ? filter : "", sort) }))
        .filter((section) => !filtering || section.trees.length > 0),
    [sections, filter, filtering, sort],
  );

  const missingTitle = (path: string) => {
    const volume = sections.find((section) =>
      section.volume.folders.some((folder) => folder.path === path || isBelow(path, folder.path)) ||
      section.volume.roots.some((root) => root === path || isBelow(path, root)),
    )?.volume;
    const offlineVolume = volume && !volume.online ? volume.name : offline.find((f) => f.path === path)?.volume;
    return offlineVolume
      ? t("offline.folders.onVolume", { path, volume: offlineVolume })
      : t("offline.folders.missing", { path });
  };
  const context: RowContext = {
    current,
    onNavigate,
    sessionRoot,
    actions,
    // A filtered list shows the matches inside their parents.
    allOpen: filtering,
    // Session folders are always counted (folders.rs); elsewhere on request.
    showCounts: countsWanted || !!sessionRoot,
    offline,
    missingTitle,
  };
  const rows = (list: FolderTree[], section: string) =>
    list.map((tree) => <FolderRow key={`${section}-${tree.path}`} node={tree} context={context} />);
  const toggleVolume = (key: string) =>
    setCollapsedVolumes((previous) => {
      const next = new Set(previous);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  const setSort = (next: FolderTreeSort) => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (appSettings) handleSettingsChange({ ...appSettings, folderTreeSort: next });
  };
  const sortMenu = (x: number, y: number) =>
    showContextMenu(x, y, [
      ...SORT_KEYS.map((key) => ({
        label: t(`studio.library.folders.sort.${key}`),
        icon: sort.key === key ? Check : undefined,
        onClick: () => setSort({ key, order: sort.order }),
      })),
      { type: OPTION_SEPARATOR },
      {
        label: t("studio.library.folders.sort.ascending"),
        icon: sort.order === SortDirection.Ascending ? Check : undefined,
        onClick: () => setSort({ ...sort, order: SortDirection.Ascending }),
      },
      {
        label: t("studio.library.folders.sort.descending"),
        icon: sort.order === SortDirection.Descending ? Check : undefined,
        onClick: () => setSort({ ...sort, order: SortDirection.Descending }),
      },
    ]);
  const nothing = !trees.length && !pinnedTrees.length && !(catalog && sections.length);
  const nothingShown = catalog ? !shownSections.length : !shownTrees.length;

  return (
    <ToolGroup
      title="Folders"
      label={t("studio.library.folders.title")}
      actions={
        <>
          <button
            type="button"
            title={t("studio.library.folders.sortFolders")}
            aria-label={t("studio.library.folders.sortFolders")}
            onClick={(e) => {
              e.preventDefault();
              const rect = e.currentTarget.getBoundingClientRect();
              sortMenu(rect.left, rect.bottom);
            }}
          >
            <ArrowDownAZ size={12} />
          </button>
          {actions?.addFolder && (
            <button
              type="button"
              title={t("studio.library.folders.addFolderTitle")}
              aria-label={t("studio.library.folders.addFolder")}
              onClick={(e) => {
                e.preventDefault();
                actions.addFolder?.();
              }}
            >
              <FolderPlus size={12} />
            </button>
          )}
        </>
      }
    >
      {!nothing && (
        <input
          className="studio-metadata-input studio-preset-search"
          aria-label={t("studio.library.folders.find")}
          placeholder={t("studio.library.folders.find")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Escape") setQuery("");
          }}
        />
      )}
      <div className="studio-folder-tree">
        {shownPinned.length > 0 && (
          <>
            <div className="studio-folder-section">{t("studio.library.folders.favorites")}</div>
            {rows(shownPinned, "pinned")}
            {!catalog && <div className="studio-folder-section">{sessionRoot ? t("studio.library.folders.session") : t("studio.library.folders.library")}</div>}
          </>
        )}
        {catalog
          ? shownSections.map(({ volume, trees: volumeTrees }) => {
              const collapsed = !filtering && collapsedVolumes.has(volume.key);
              return (
                <Fragment key={`volume-${volume.key}`}>
                  <VolumeHeader volume={volume} collapsed={collapsed} onToggle={() => toggleVolume(volume.key)} />
                  {!collapsed && rows(volumeTrees, volume.key)}
                </Fragment>
              );
            })
          : rows(shownTrees, "library")}
        {unlisted.length > 0 && (
          <>
            <div className="studio-folder-section">{t("offline.folders.title")}</div>
            {unlisted.map((folder) => (
              <div
                key={`offline-${folder.path}`}
                className={`studio-folder is-offline ${current === folder.path ? "is-selected" : ""}`}
                style={{ paddingLeft: 10 }}
              >
                <button disabled aria-hidden tabIndex={-1}>
                  <ChevronRight size={11} />
                </button>
                <button
                  onClick={() => onNavigate(folder.path)}
                  title={folder.missing ? missingTitle(folder.path) : folder.path}
                >
                  {folder.missing ? <FolderX size={14} /> : <Folder size={14} />}
                  <span>{folder.path.split(/[\\/]/).pop()}</span>
                  <small className="studio-folder-offline">{`?${folder.count}`}</small>
                </button>
              </div>
            ))}
          </>
        )}
      </div>
      {filtering && nothingShown && !shownPinned.length && (
        <p className="studio-help">{t("studio.library.folders.noMatch", { filter })}</p>
      )}
      {nothing && <p className="studio-help">{loading ? t("studio.library.folders.reading") : t("studio.library.folders.empty")}</p>}
      {actions?.addFolder && (
        <button className="studio-text-button" onClick={() => actions.addFolder?.()}>
          {t("studio.library.folders.addFolderButton")}
        </button>
      )}
    </ToolGroup>
  );
}

export default function StudioNavigation({
  onNavigate,
  onSelectAlbum,
  folderActions,
}: {
  onNavigate(path: string): void;
  onSelectAlbum?: SelectAlbum;
  folderActions?: FolderActions;
}) {
  const { t } = useTranslation();
  const image = useEditorStore((s) => s.selectedImage);
  const preview = useProcessStore((s) =>
    image ? s.thumbnails[image.path] : undefined,
  );
  return (
    <ToolColumn scope="library" className="studio-navigation">
      <ToolGroup key="navigator" title="Navigator" label={t("studio.library.navigator.title")}>
        <div className="studio-navigator">
          {preview ? (
            <img src={preview} alt={image?.path.split("/").pop()} />
          ) : (
            <span>{t("studio.library.navigator.empty")}</span>
          )}
        </div>
        {image && (
          <div className="studio-nav-caption">
            {image.path.split("/").pop()}
          </div>
        )}
      </ToolGroup>
      <StudioFolders key="folders" onNavigate={onNavigate} actions={folderActions} />
      {onSelectAlbum && <StudioAlbums key="albums" onSelectAlbum={onSelectAlbum} />}
      <StudioKeywords key="keywords" />
      <StudioMetadataTool key="metadata" />
      <StudioExif key="exif" />
    </ToolColumn>
  );
}
