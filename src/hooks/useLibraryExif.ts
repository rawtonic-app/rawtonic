import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLibraryStore } from "../store/useLibraryStore";

// Filename/date/rating browsing does not need to read every RAW. The selected
// image loads its own EXIF; only explicit EXIF sorts/queries hydrate the library.
export function useLibraryExif(required: boolean) {
  const folder = useLibraryStore((s) => s.currentFolderPath);
  const album = useLibraryStore((s) => s.activeAlbumId);
  const count = useLibraryStore((s) => s.imageList.length);
  const missing = useLibraryStore(
    (s) => required && s.imageList.some((image) => image.exif == null),
  );
  useEffect(() => {
    if (!required || !missing) return;
    let cancelled = false;
    const paths = useLibraryStore
      .getState()
      .imageList.filter((image) => image.exif == null)
      .map((image) => image.path);
    void (async () => {
      for (let i = 0; i < paths.length && !cancelled; i += 100) {
        const chunk = paths.slice(i, i + 100);
        try {
          const data = await invoke<Record<string, Record<string, string>>>(
            "read_exif_for_paths",
            { paths: chunk },
          );
          if (cancelled) return;
          const requested = new Set(chunk);
          useLibraryStore.getState().setLibrary((state) => ({
            imageList: state.imageList.map((image) =>
              requested.has(image.path)
                ? { ...image, exif: data[image.path] ?? {} }
                : image,
            ),
          }));
        } catch (error) {
          console.error("Failed to load requested library EXIF:", error);
          return;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [required, missing, folder, album, count]);
}
