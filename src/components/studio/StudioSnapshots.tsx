import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Camera, Columns2, ImageOff, Pencil, Plus, RefreshCw, X } from "lucide-react";
import { notify } from "../../activity/notify";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useSnapshotStore, type Snapshot } from "../../store/useSnapshotStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { splitViewAvailable } from "../../utils/beforeAfter";
import {
  beforeSnapshot,
  compareBlocker,
  compareWithSnapshot,
  comparisonSignature,
} from "../../utils/beforeSnapshot";
import { defaultSnapshotName, restoreSnapshot, takeSnapshot } from "../../utils/snapshots";
import { ToolGroup } from "./StudioDevelop";

/** Lightroom's snapshots, with Capture One's variant thumbnails: named
 * states of the open photograph's development. A click restores one as a
 * history step (undoable); + takes one of the current edit (the history
 * takes one of any step); the split compares one with the current edit
 * (Before/After's before becomes it: Lightroom's snapshot copied to
 * Before) while it frames and develops the photograph as the edit does;
 * the pencil renames, the arrows update one with the current
 * edit. Open from the start, in the dock under the tools. Kept by the
 * library (the editor sidecar for a photograph in none). */
export default function StudioSnapshots() {
  const { t, i18n } = useTranslation();
  const image = useEditorStore((s) => s.selectedImage);
  const panelsReady = useEditorStore(panelsShowReady);
  const snapshots = useSnapshotStore((s) => s.snapshots);
  const thumbnails = useSnapshotStore((s) => s.thumbnails);
  const listed = useSnapshotStore((s) => s.path);
  const [editing, setEditing] = useState<string | null>(null);
  const path = image?.isReady ? image.path : null;
  const { toggleSplitView } = useEditorActions();
  useEditorStore((s) => s.beforeSnapshot);
  const signature = useEditorStore((s) => comparisonSignature(s.adjustments, s.selectedImage?.isRaw ?? false));
  const compared = beforeSnapshot();
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const splitAvailable = useSettingsStore((s) =>
    splitViewAvailable(s.appSettings?.useWgpuRenderer, s.osPlatform),
  );
  /** Compares with `snapshot` in the split (turned on for it), or, when it
   * is compared with already, with the unedited photograph again. */
  const compare = (snapshot: Snapshot) => {
    if (compared?.id === snapshot.id) return compareWithSnapshot(null);
    compareWithSnapshot(snapshot);
    if (!useEditorStore.getState().splitView) toggleSplitView();
  };

  useEffect(() => {
    // The list follows the open photograph once it is ready.
    if (path === null && useSnapshotStore.getState().path === null) return;
    useSnapshotStore
      .getState()
      .load(path)
      .catch((error) => notify.error(t("studio.develop.snapshots.failed", { error: String(error) })));
  }, [path, t]);

  const create = async () => {
    const { adjustments } = useEditorStore.getState();
    try {
      const taken = await takeSnapshot(defaultSnapshotName(new Date(), i18n.language), adjustments);
      if (taken) setEditing(taken.id);
    } catch (error) {
      notify.error(t("studio.develop.snapshots.failed", { error: String(error) }));
    }
  };

  return (
    <ToolGroup
      title="Snapshots"
      label={t("studio.develop.snapshots.title")}
      // Folded at first, so the dock takes two header rows from the tools
      // on a laptop screen; it remembers being opened.
      open={false}
      actions={
        <button
          type="button"
          title={t("studio.develop.snapshots.create")}
          aria-label={t("studio.develop.snapshots.create")}
          disabled={!panelsReady || !path}
          onClick={(e) => {
            e.preventDefault();
            const details = e.currentTarget.closest("details");
            if (details) details.open = true;
            void create();
          }}
        >
          <Plus size={12} />
        </button>
      }
    >
      {path && listed === path && snapshots.length > 0 ? (
        <ul className="studio-snapshots">
          {snapshots.map((snapshot) => (
            <SnapshotRow
              key={snapshot.id}
              snapshot={snapshot}
              path={path}
              thumbnail={thumbnails[snapshot.id]}
              disabled={!panelsReady}
              editing={editing === snapshot.id}
              onEdit={(on) => setEditing(on ? snapshot.id : null)}
              compared={compared?.id === snapshot.id}
              canCompare={splitAvailable}
              blocker={image ? compareBlocker(snapshot, signature, image.isRaw, osPlatform) : null}
              onCompare={() => compare(snapshot)}
            />
          ))}
        </ul>
      ) : (
        <p className="studio-help">
          {path ? t("studio.develop.snapshots.empty") : t("studio.develop.history.empty")}
        </p>
      )}
    </ToolGroup>
  );
}

function SnapshotRow({
  snapshot,
  path,
  thumbnail,
  disabled,
  editing,
  onEdit,
  compared,
  canCompare,
  blocker,
  onCompare,
}: {
  snapshot: Snapshot;
  path: string;
  /** Object URL; empty when it could not be developed, absent until it is. */
  thumbnail: string | undefined;
  disabled: boolean;
  editing: boolean;
  onEdit(on: boolean): void;
  /** Before/After's before is this snapshot. */
  compared: boolean;
  /** The split can show (`splitViewAvailable`). */
  canCompare: boolean;
  /** Why it cannot be Before beside the edit now (`compareBlocker`). */
  blocker: "differs" | "aiDenoise" | null;
  onCompare(): void;
}) {
  const { t, i18n } = useTranslation();
  const input = useRef<HTMLInputElement>(null);
  // Enter, Escape and leaving the field each end the rename once.
  const ended = useRef(false);
  useEffect(() => {
    if (!editing) return;
    ended.current = false;
    input.current?.focus();
    input.current?.select();
  }, [editing]);
  const fail = (error: unknown) =>
    notify.error(t("studio.develop.snapshots.failed", { error: String(error) }));
  const rename = (name: string | null) => {
    if (ended.current) return;
    ended.current = true;
    onEdit(false);
    if (name !== null && name.trim() && name.trim() !== snapshot.name)
      useSnapshotStore.getState().rename(path, snapshot.id, name).catch(fail);
  };
  const thumb = (
    <span className="studio-snapshot-thumb">
      {thumbnail ? (
        <img src={thumbnail} alt="" />
      ) : thumbnail === "" ? (
        <ImageOff size={14} aria-hidden />
      ) : (
        <Camera size={14} aria-hidden />
      )}
    </span>
  );
  const taken = new Date(snapshot.createdAt).toLocaleString(i18n.language, {
    dateStyle: "medium",
    timeStyle: "short",
  });
  return (
    <li className="studio-snapshot">
      {editing ? (
        // Renaming: a field in place of the row, which restores on a click.
        <div className="studio-snapshot-restore">
          {thumb}
          <span className="studio-snapshot-text">
            <input
              ref={input}
              defaultValue={snapshot.name}
              aria-label={t("studio.develop.snapshots.name")}
              maxLength={200}
              onBlur={(e) => rename(e.currentTarget.value)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") rename(e.currentTarget.value);
                if (e.key === "Escape") rename(null);
              }}
            />
            <span className="studio-snapshot-time">{taken}</span>
          </span>
        </div>
      ) : (
        <button
          type="button"
          className="studio-snapshot-restore"
          disabled={disabled}
          title={t("studio.develop.snapshots.restore", { name: snapshot.name })}
          onClick={() => restoreSnapshot(snapshot)}
        >
          {thumb}
          <span className="studio-snapshot-text">
            <span className="studio-snapshot-name">{snapshot.name}</span>
            <span className="studio-snapshot-time">{taken}</span>
          </span>
        </button>
      )}
      <button
        type="button"
        className="studio-snapshot-compare"
        disabled={disabled || !canCompare || blocker !== null}
        aria-pressed={compared}
        title={t(
          blocker === "aiDenoise"
            ? "studio.develop.snapshots.compareAiDenoise"
            : blocker === "differs"
              ? "studio.develop.snapshots.compareDiffers"
              : compared
                ? "studio.develop.snapshots.compareEnd"
                : "studio.develop.snapshots.compare",
          { name: snapshot.name },
        )}
        aria-label={t("studio.develop.snapshots.compare", { name: snapshot.name })}
        onClick={onCompare}
      >
        <Columns2 size={12} />
      </button>
      <button
        type="button"
        className="studio-snapshot-rename"
        disabled={disabled}
        title={t("studio.develop.snapshots.rename", { name: snapshot.name })}
        aria-label={t("studio.develop.snapshots.rename", { name: snapshot.name })}
        onClick={() => onEdit(true)}
      >
        <Pencil size={12} />
      </button>
      <button
        type="button"
        className="studio-snapshot-update"
        disabled={disabled}
        title={t("studio.develop.snapshots.update", { name: snapshot.name })}
        aria-label={t("studio.develop.snapshots.update", { name: snapshot.name })}
        onClick={() =>
          useSnapshotStore
            .getState()
            .update(path, snapshot.id, useEditorStore.getState().adjustments)
            .catch(fail)
        }
      >
        <RefreshCw size={12} />
      </button>
      <button
        type="button"
        className="studio-snapshot-delete"
        disabled={disabled}
        title={t("studio.develop.snapshots.delete", { name: snapshot.name })}
        aria-label={t("studio.develop.snapshots.delete", { name: snapshot.name })}
        onClick={() => useSnapshotStore.getState().remove(path, snapshot.id).catch(fail)}
      >
        <X size={12} />
      </button>
    </li>
  );
}
