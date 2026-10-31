import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../ui/AppProperties";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useActivityStore } from "../../store/useActivityStore";
import { isBelow } from "../../utils/catalogFolders";
import { IMPORTS_CHANGED } from "../../utils/folderSync";
import { bannerDismissed, dismissBanner, requestAddFolder, type FolderPlan } from "./addFolder";

/**
 * A catalog's listed folder (a browse root added before Add Folder… added
 * photographs, G39 AF) whose photographs are shown but not in the catalog:
 * not counted, searched, in smart albums or exported. The banner says so
 * with their count and offers to add them (the Add Folder dialog); "Not
 * now" puts it away for that folder for good, so it is offered once and
 * nothing is ever added silently.
 */
export default function UnaddedFolderBanner() {
  const { t } = useTranslation();
  const library = useLibraryStore((s) => s.sessionDirectory);
  const catalog = useLibraryStore((s) => !!s.catalogFolders?.catalog);
  const roots = useLibraryStore((s) => s.rootPaths);
  const current = useLibraryStore((s) => s.currentFolderPath);
  // While an import runs its count is stale; it is read again when it ends.
  const importing = useActivityStore((s) =>
    s.entries.some((e) => e.source === "import" && e.status === "running"),
  );
  const root =
    library && catalog && current
      ? (roots.find((r) => r !== library && !isBelow(r, library) && (r === current || isBelow(current, r))) ?? null)
      : null;
  const [found, setFound] = useState<{ root: string; count: number } | null>(null);
  const [dismissed, setDismissed] = useState(0);

  useEffect(() => {
    setFound(null);
    if (!library || !root || bannerDismissed(library, root)) return;
    let live = true;
    const scan = () =>
      invoke<FolderPlan>(Invokes.CatalogAddFolderScan, { directory: library, folder: root, subfolders: true })
        .then((plan) => live && setFound({ root, count: plan.photographs }))
        .catch(() => live && setFound(null));
    void scan();
    window.addEventListener(IMPORTS_CHANGED, scan);
    return () => {
      live = false;
      window.removeEventListener(IMPORTS_CHANGED, scan);
    };
  }, [library, root, dismissed]);

  if (importing || !library || !root || found?.root !== root || !found.count) return null;
  const name = root.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || root;
  return (
    <div className="studio-unadded-banner" role="status">
      <p>{t("session.addFolder.banner", { count: found.count, name })}</p>
      <button
        type="button"
        className="is-primary"
        onClick={() => requestAddFolder({ folder: root, show: async () => {} })}
      >
        {t("session.addFolder.bannerAdd")}
      </button>
      <button
        type="button"
        onClick={() => {
          dismissBanner(library, root);
          setDismissed((n) => n + 1);
        }}
      >
        {t("session.addFolder.bannerLater")}
      </button>
    </div>
  );
}
