import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { BookImage, Folder } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "react-toastify";
import { Invokes } from "../ui/AppProperties";
import { childrenOf, SESSION_ALBUMS_CHANGED, type SessionAlbum } from "./StudioAlbums";

/** "Add to Album" for a session: its own albums (SQLite), read when the
 * submenu opens, groups shown as headings. */
export default function SessionAlbumSubMenu({
  directory,
  paths,
  hideContextMenu,
}: {
  directory: string;
  paths: string[];
  hideContextMenu(): void;
}) {
  const { t } = useTranslation();
  const [albums, setAlbums] = useState<SessionAlbum[] | null>(null);
  useEffect(() => {
    let current = true;
    invoke<SessionAlbum[]>(Invokes.SessionAlbums, { directory })
      .then((list) => current && setAlbums(list ?? []))
      .catch((e) => {
        if (current) setAlbums([]);
        toast.error(t("studio.library.albums.toasts.readFailed", { error: String(e) }));
      });
    return () => {
      current = false;
    };
  }, [directory, t]);
  const add = async (album: SessionAlbum) => {
    hideContextMenu();
    try {
      await invoke(Invokes.SessionAlbumAdd, { directory, id: album.id, paths });
      window.dispatchEvent(new CustomEvent(SESSION_ALBUMS_CHANGED));
      toast.success(
        t("studio.library.albumMenu.added", {
          count: paths.length,
          n: paths.length.toLocaleString(),
          album: album.name,
        }),
      );
    } catch (e) {
      toast.error(t("studio.library.albumMenu.addFailed", { album: album.name, error: String(e) }));
    }
  };
  const rows = (parent: string | null, depth: number): React.ReactNode[] =>
    childrenOf(albums ?? [], parent).flatMap((album) =>
      album.kind === "group"
        ? [
            <div
              key={album.id}
              className="rawtonic-menu-heading"
              style={{ paddingLeft: 8 + depth * 12 }}
            >
              <Folder size={13} />
              <span>{album.name}</span>
            </div>,
            ...rows(album.id, depth + 1),
          ]
        : [
            <button
              key={album.id}
              type="button"
              role="menuitem"
              className="rawtonic-menu-item"
              style={{ paddingLeft: 8 + depth * 12 }}
              onClick={() => void add(album)}
            >
              <span className="flex items-center gap-2.5 min-w-0">
                <BookImage size={14} className="shrink-0" />
                <span className="truncate">{album.name}</span>
              </span>
              <small className="rawtonic-menu-count">{album.count}</small>
            </button>,
          ],
    );
  return (
    <div className="rawtonic-menu-list w-56" role="menu" aria-label={t("studio.library.albumMenu.label")}>
      {albums === null ? (
        <div className="rawtonic-menu-heading">{t("studio.library.albumMenu.reading")}</div>
      ) : albums.some((album) => album.kind !== "group") ? (
        rows(null, 0)
      ) : (
        <div className="rawtonic-menu-heading">{t("studio.library.albumMenu.empty")}</div>
      )}
    </div>
  );
}
