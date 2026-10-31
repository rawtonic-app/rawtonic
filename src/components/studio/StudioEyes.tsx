// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { CircleQuestionMark, Eye, EyeClosed, Glasses } from "lucide-react";
import { notify } from "../../activity/notify";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import {
  effectiveEyes,
  setFaceEyes,
  useEyesStore,
  type EyeCall,
  type EyeState,
  type EyeSummary,
  type PhotoEyes,
} from "../../store/useEyesStore";
import { Invokes } from "../ui/AppProperties";

/**
 * Eyes per face for culling (`face_eyes`): a badge on each photograph with
 * a face whose eyes are closed or need a check, the Loupe's summary and its
 * faces with their eyes, each call open to the photographer's own. Closed or
 * partly closed is amber, not visible grey, uncertain an outlined "?"; open
 * eyes need no badge. A suggestion only: nothing is rejected or removed.
 */

const ICONS: Record<EyeState, typeof Eye> = {
  open: Eye,
  closed: EyeClosed,
  notVisible: Glasses,
  uncertain: CircleQuestionMark,
};

function stateLabel(t: TFunction, state: EyeState) {
  switch (state) {
    case "open":
      return t("studio.eyes.state.open");
    case "closed":
      return t("studio.eyes.state.closed");
    case "notVisible":
      return t("studio.eyes.state.notVisible");
    default:
      return t("studio.eyes.state.uncertain");
  }
}

/** "Faces 14 · eyes closed 2 · check 3 · not visible 1", what applies. */
export function eyeSummaryText(t: TFunction, summary: EyeSummary): string {
  if (summary.faces === 0) return t("studio.eyes.noFaces");
  const parts = [t("studio.eyes.summary.faces", { n: summary.faces.toLocaleString() })];
  if (summary.closed) parts.push(t("studio.eyes.summary.closed", { n: summary.closed.toLocaleString() }));
  if (summary.uncertain) parts.push(t("studio.eyes.summary.check", { n: summary.uncertain.toLocaleString() }));
  if (summary.notVisible)
    parts.push(t("studio.eyes.summary.notVisible", { n: summary.notVisible.toLocaleString() }));
  return parts.join(" · ");
}

/** A grid tile's or compare cell's badge: closed eyes (amber, "2/14") or,
 * without any, faces that need a check ("?"). Nothing otherwise. */
export function PhotoEyesBadge({ path, compact }: { path: string; compact?: boolean }) {
  const { t } = useTranslation();
  const summary = useEyesStore((s) => s.summaries[path]);
  if (!summary || (!summary.closed && !summary.uncertain)) return null;
  const closed = summary.closed > 0;
  const Icon = closed ? EyeClosed : CircleQuestionMark;
  const text = eyeSummaryText(t, summary);
  return (
    <span
      className={`studio-eyes-badge ${closed ? "is-closed" : "is-uncertain"}`}
      role="img"
      aria-label={text}
      title={`${text}\n${t("studio.eyes.suggestion")}`}
    >
      <Icon size={11} />
      {!compact && <span>{closed ? `${summary.closed}/${summary.faces}` : summary.uncertain}</span>}
    </span>
  );
}

/** The open photograph's eyes and faces, read from the catalog again after
 * every reading or call. */
function usePhotoEyes(path: string | undefined) {
  const directory = useLibraryStore((s) => s.sessionDirectory);
  const enabled = useSettingsStore((s) => !!s.appSettings?.eyeCheck);
  const generation = useEyesStore((s) => s.generation);
  const [eyes, setEyes] = useState<{ path: string; eyes: PhotoEyes } | null>(null);
  useEffect(() => {
    if (!directory || !path || !enabled) return;
    let current = true;
    invoke<PhotoEyes>(Invokes.PhotoEyes, { directory, path })
      .then((read) => current && setEyes({ path, eyes: read }))
      .catch(() => current && setEyes(null));
    return () => {
      current = false;
    };
  }, [directory, path, enabled, generation]);
  return enabled && directory && eyes && eyes.path === path ? { directory, eyes: eyes.eyes } : null;
}

/** The Loupe's faces: each face large enough to see its eyes, with its
 * call; a click lets the photographer make their own (or give the reading
 * back). Shown for photographs with faces once eyes are checked. */
export function LoupeFaces({ path }: { path?: string }) {
  const { t } = useTranslation();
  const read = usePhotoEyes(path);
  const faces = read?.eyes.faces;
  const [crops, setCrops] = useState<{ key: string; urls: string[] } | null>(null);
  // The open call menu: its face and where it opens (above the button: the
  // strip sits at the bottom of the Loupe).
  const [menu, setMenu] = useState<{ index: number; left: number; bottom: number } | null>(null);
  const strip = useRef<HTMLDivElement>(null);
  // Crops follow the boxes, not the calls: a call keeps its picture.
  const key = useMemo(
    () => (path && faces?.length ? `${path}\n${JSON.stringify(faces.map((f) => f.bbox))}` : ""),
    [path, faces],
  );
  useEffect(() => {
    if (!key || !path || !faces) return;
    let current = true;
    invoke<string[]>(Invokes.FaceEyeCrops, { path, boxes: faces.map((f) => f.bbox) })
      .then((urls) => current && setCrops({ key, urls }))
      .catch(() => current && setCrops({ key, urls: [] }));
    return () => {
      current = false;
    };
    // `faces` changes with every call; `key` only with the boxes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  useEffect(() => setMenu(null), [path]);
  if (!read || !path || !faces?.length) return null;
  const summary: EyeSummary = {
    faces: faces.length,
    closed: faces.filter((f) => effectiveEyes(f) === "closed").length,
    notVisible: faces.filter((f) => effectiveEyes(f) === "notVisible").length,
    uncertain: faces.filter((f) => effectiveEyes(f) === "uncertain").length,
  };
  const urls = crops?.key === key ? crops.urls : [];
  const call = (face: number, state: EyeCall | null) => {
    setMenu(null);
    setFaceEyes(read.directory, path, face, state).catch((error) =>
      notify.error(t("studio.eyes.callFailed", { error: String(error) })),
    );
  };
  return (
    <div
      className="studio-loupe-faces"
      ref={strip}
      role="group"
      aria-label={t("studio.eyes.faces")}
      onKeyDown={(e) => {
        if (e.key === "Escape" && menu !== null) {
          e.stopPropagation();
          setMenu(null);
        }
      }}
      onBlur={(e) => {
        if (!strip.current?.contains(e.relatedTarget as Node | null)) setMenu(null);
      }}
    >
      <span className="studio-loupe-faces-summary" title={t("studio.eyes.suggestion")}>
        {eyeSummaryText(t, summary)}
      </span>
      <div className="studio-loupe-faces-list" role="list">
        {faces.map((face, index) => {
          const state = effectiveEyes(face);
          const Icon = ICONS[state];
          const label = stateLabel(t, state);
          const title = face.overrideState
            ? t("studio.eyes.yourCall", { state: label, read: stateLabel(t, face.state) })
            : t("studio.eyes.asRead", { state: label });
          return (
            <div key={face.face} role="listitem" className={`studio-loupe-face is-${state}`}>
              {urls[index] ? <img src={urls[index]} alt="" draggable={false} /> : <span className="studio-loupe-face-empty" />}
              <button
                className={`studio-eye-call is-${state} ${face.overrideState ? "is-own" : ""}`}
                aria-label={t("studio.eyes.faceLabel", { n: index + 1, state: label })}
                aria-haspopup="menu"
                aria-expanded={menu?.index === index}
                title={title}
                onClick={(e) => {
                  const rect = e.currentTarget.getBoundingClientRect();
                  setMenu(
                    menu?.index === index
                      ? null
                      : { index, left: rect.left, bottom: window.innerHeight - rect.top + 6 },
                  );
                }}
              >
                <Icon size={12} />
              </button>
              {menu?.index === index && (
                <div
                  className="studio-eye-menu"
                  role="menu"
                  aria-label={t("studio.eyes.callMenu")}
                  style={{ left: menu.left, bottom: menu.bottom }}
                >
                  {(["open", "closed", "notVisible"] as const).map((option) => {
                    const OptionIcon = ICONS[option];
                    return (
                      <button
                        key={option}
                        role="menuitemradio"
                        aria-checked={face.overrideState === option}
                        onClick={() => call(face.face, option)}
                      >
                        <OptionIcon size={12} />
                        {stateLabel(t, option)}
                      </button>
                    );
                  })}
                  <button
                    role="menuitemradio"
                    aria-checked={face.overrideState === null}
                    onClick={() => call(face.face, null)}
                  >
                    {t("studio.eyes.useReading", { state: stateLabel(t, face.state) })}
                  </button>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
