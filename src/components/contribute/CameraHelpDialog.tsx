// SPDX-License-Identifier: AGPL-3.0-or-later
// "Help support this camera" (ADR 0017, channel 3): offered when a RAW
// cannot be developed (an unknown camera or RAW mode, or a damaged file).
// A short guide to sharing a sample on raw.pixls.us, the public collection
// open-source RAW decoders learn new cameras from, opened in the browser —
// the app never uploads there itself — or, instead, a problem report that
// can carry the file privately to Rawtonic's makers.
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-shell";
import { ExternalLink } from "lucide-react";
import { toast } from "react-toastify";
import { useModalFocus } from "../../hooks/useModalFocus";
import { useUIStore, type CameraHelpRequest } from "../../store/useUIStore";
import { Invokes } from "../ui/AppProperties";
import Button from "../ui/Button";
import Text from "../ui/Text";
import { TextVariants } from "../../types/typography";
import { isUnreadableError } from "../../utils/loadError";
import { photoInfo, type PhotoInfo } from "./reports";

/** raw.pixls.us: its start page holds the upload form, `#repo` the list of
 * cameras and modes with samples. There is no machine-readable listing, so
 * the app links the list instead of reading it. */
export const RAW_PIXLS_URL = "https://raw.pixls.us/";
export const RAW_PIXLS_LIST_URL = "https://raw.pixls.us/#repo";

/** Whether a load error is the decoder's (a file it cannot read) rather
 * than a missing file or a cancelled load (`isUnreadableError`). A format
 * no open decoder reads (Nikon's High Efficiency NEF; its error says so,
 * `NO_OPEN_DECODER` in crates/engine/src/image_loader.rs) is left out: a
 * sample does not help there. */
export const isDecoderError = (error: unknown) =>
  isUnreadableError(error) && !/no open decoder/i.test(String(error));

function CameraHelpNotice({ text, request }: { text: string; request: CameraHelpRequest }) {
  const { t } = useTranslation();
  return (
    <>
      <span className="block">{text}</span>
      <button
        type="button"
        className="mt-1 underline text-sm"
        onClick={() => useUIStore.getState().setCameraHelp(request)}
      >
        {t("contribute.cameraHelp.offer")}
      </button>
    </>
  );
}

/** A toast that says `text` and offers camera help for the file. */
export const offerCameraHelp = (text: string, request: CameraHelpRequest) => {
  const show = request.reason === "preview" ? toast.warn : toast.error;
  show(<CameraHelpNotice text={text} request={request} />, { toastId: `camera-help:${request.path}` });
};

const readCamera = async (path: string): Promise<PhotoInfo | null> => {
  const exif = await invoke<Record<string, Record<string, string>>>(Invokes.ReadExifForPaths, { paths: [path] });
  return photoInfo({ path, exif: exif[path] ?? null });
};

interface CameraHelpDialogProps {
  request: CameraHelpRequest | null;
  onClose(): void;
  /** "Report a problem with this file": the problem report for it. */
  onReport(photo: PhotoInfo | null): void;
  /** Tests replace the app's calls. */
  openUrl?(url: string): Promise<void>;
  camera?(path: string): Promise<PhotoInfo | null>;
}

export default function CameraHelpDialog({
  request,
  onClose,
  onReport,
  openUrl = open,
  camera = readCamera,
}: CameraHelpDialogProps) {
  const { t } = useTranslation();
  const [photo, setPhoto] = useState<PhotoInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const panel = useRef<HTMLDivElement>(null);
  useModalFocus(panel, request !== null);

  useEffect(() => {
    setPhoto(null);
    setError(null);
    if (!request) return;
    let live = true;
    camera(request.path)
      .then((info) => live && setPhoto(info))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [camera, request]);

  if (!request) return null;

  const visit = async (url: string) => {
    try {
      await openUrl(url);
    } catch {
      setError(t("contribute.report.openFailed"));
    }
  };

  const name = request.path.split("?")[0].split(/[\\/]/).pop() ?? "";
  const content = (
    <div
      aria-labelledby="camera-help-title"
      aria-modal="true"
      className="fixed inset-0 flex items-center justify-center z-[9999] bg-black/30 backdrop-blur-xs"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          onClose();
        }
      }}
      role="dialog"
    >
      <div
        className="bg-surface rounded-lg shadow-xl p-6 w-full max-w-2xl max-h-[85vh] flex flex-col text-text-primary"
        ref={panel}
        tabIndex={-1}
      >
        <Text variant={TextVariants.title} id="camera-help-title" className="mb-3">
          {t("contribute.cameraHelp.title")}
        </Text>
        <div className="min-h-0 flex-1 overflow-auto flex flex-col gap-3 pr-1">
          <Text>
            {request.reason === "preview"
              ? t("contribute.cameraHelp.preview", { name })
              : t("contribute.cameraHelp.unreadable", { name })}
          </Text>
          {photo && (
            <Text color="secondary">
              {t("contribute.cameraHelp.camera", { camera: `${photo.make} ${photo.model}`, type: photo.fileType })}
            </Text>
          )}
          <Text>{t("contribute.cameraHelp.why")}</Text>
          <ol className="list-decimal ml-6 flex flex-col gap-2 text-sm">
            <li>
              {t("contribute.cameraHelp.check")}{" "}
              <button type="button" className="underline" onClick={() => void visit(RAW_PIXLS_LIST_URL)}>
                {t("contribute.cameraHelp.openList")}
              </button>
            </li>
            <li>{t("contribute.cameraHelp.shot")}</li>
            <li>{t("contribute.cameraHelp.copy")}</li>
            <li>{t("contribute.cameraHelp.upload")}</li>
          </ol>
          <Text variant={TextVariants.small} color="secondary">
            {t("contribute.cameraHelp.nothingSent")}
          </Text>
          <div className="rounded bg-bg-primary p-3 text-sm flex flex-col gap-2 items-start">
            <span>{t("contribute.cameraHelp.reportInstead")}</span>
            <Button variant="ghost" onClick={() => onReport(photo)}>
              {t("contribute.cameraHelp.report")}
            </Button>
          </div>
          {error && (
            <Text color="error" role="alert">
              {error}
            </Text>
          )}
        </div>
        <div className="flex justify-end gap-3 mt-4">
          <Button
            className="bg-bg-primary shadow-transparent hover:bg-bg-primary text-white shadow-none"
            variant="ghost"
            onClick={onClose}
            data-autofocus
          >
            {t("contribute.review.close")}
          </Button>
          <Button onClick={() => void visit(RAW_PIXLS_URL)}>
            <span className="flex items-center gap-2">
              <ExternalLink size={16} />
              {t("contribute.cameraHelp.openUpload")}
            </span>
          </Button>
        </div>
      </div>
    </div>
  );
  return typeof document !== "undefined" ? createPortal(content, document.body) : content;
}
