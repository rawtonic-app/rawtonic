// SPDX-License-Identifier: AGPL-3.0-or-later
// "Report a problem" (ADR 0017, channel 1): the user's text plus, each
// optional, system details, the log's last lines (editable), the open
// photo's camera and file type, a screenshot of the window taken when the
// dialog opened, and the photo's file itself (off by default; sent as it is,
// with its metadata). Nothing is sent before the review's Send.
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import Text from "../ui/Text";
import { TextVariants } from "../../types/typography";
import { formatFileSize } from "../../utils/formatFileSize";
import ReportDialog from "./ReportDialog";
import {
  DIAGNOSTICS_CAP,
  TEXT_CAP,
  reportPayload,
  type CrashRecord,
  type PhotoInfo,
  type ReportContext,
  type Screenshot,
} from "./reports";
import type { ContributionChannel, ContributionReceipt, PreparedContribution } from "./relay";

interface ProblemReportDialogProps {
  isOpen: boolean;
  onClose(): void;
  /** Null while it is being read. */
  context: ReportContext | null;
  screenshot: Screenshot | null;
  photo: PhotoInfo | null;
  /** A crash of an earlier run that was not sent, offered beside the form. */
  pendingCrash?: CrashRecord | null;
  onReviewCrash?(): void;
  /** Tests replace the app's calls. */
  prepare?(channel: ContributionChannel, payload: Record<string, unknown>): Promise<PreparedContribution>;
  available?(): Promise<boolean>;
  openUrl?(url: string): Promise<void>;
  send?(prepared: PreparedContribution): Promise<ContributionReceipt>;
}

const base64Bytes = (data: string) => Math.floor((data.length * 3) / 4) - (data.match(/=+$/)?.[0].length ?? 0);

export default function ProblemReportDialog({
  isOpen,
  onClose,
  context,
  screenshot,
  photo,
  pendingCrash,
  onReviewCrash,
  ...calls
}: ProblemReportDialogProps) {
  const { t } = useTranslation();
  const [text, setText] = useState("");
  const [includeSystem, setIncludeSystem] = useState(true);
  const [includeLog, setIncludeLog] = useState(true);
  const [logTail, setLogTail] = useState("");
  const [includePhoto, setIncludePhoto] = useState(true);
  const [includeScreenshot, setIncludeScreenshot] = useState(false);
  const [includePhotoFile, setIncludePhotoFile] = useState(false);

  useEffect(() => {
    if (!isOpen) return;
    setText("");
    setIncludeSystem(true);
    setIncludeLog(true);
    setIncludePhoto(true);
    setIncludeScreenshot(false);
    setIncludePhotoFile(false);
  }, [isOpen]);

  useEffect(() => setLogTail(context?.logTail ?? ""), [context]);

  const payload = useMemo(
    () =>
      context && text.trim()
        ? reportPayload({
            text,
            context,
            includeSystem,
            logTail: includeLog ? logTail : null,
            photo: includePhoto || includePhotoFile ? photo : null,
            photoFile: includePhotoFile && photo !== null,
            screenshot: includeScreenshot ? screenshot : null,
          })
        : null,
    [
      context,
      includeLog,
      includePhoto,
      includePhotoFile,
      includeScreenshot,
      includeSystem,
      logTail,
      photo,
      screenshot,
      text,
    ],
  );

  const gpu = context?.gpu ? [context.gpu.vendor, context.gpu.name].filter(Boolean).join(", ") : null;
  const system = context
    ? `Rawtonic ${context.app.version} · ${context.app.os} ${context.app.osVersion} (${context.app.arch})`
    : "";

  return (
    <ReportDialog
      isOpen={isOpen}
      title={t("contribute.report.title")}
      channel="report"
      payload={payload}
      onClose={onClose}
      {...calls}
    >
      <label className="flex flex-col gap-1">
        <Text>{t("contribute.report.textLabel")}</Text>
        <textarea
          className="min-h-28 rounded bg-bg-primary p-2 text-sm text-text-primary"
          data-autofocus
          maxLength={TEXT_CAP}
          onChange={(e) => setText(e.target.value)}
          placeholder={t("contribute.report.textPlaceholder")}
          value={text}
        />
      </label>
      <Text variant={TextVariants.small} color="secondary">
        {t("contribute.report.always", { system })}
      </Text>
      <label className="flex items-start gap-2">
        <input
          type="checkbox"
          checked={includeSystem && Boolean(gpu)}
          disabled={!gpu}
          onChange={(e) => setIncludeSystem(e.target.checked)}
        />
        <span>
          {t("contribute.report.system")}
          {gpu && <span className="text-text-secondary"> — {gpu}</span>}
        </span>
      </label>
      <label className="flex items-start gap-2">
        <input
          type="checkbox"
          checked={includePhoto && photo !== null}
          disabled={!photo}
          onChange={(e) => setIncludePhoto(e.target.checked)}
        />
        <span>
          {t("contribute.report.photo")}
          {photo ? (
            <span className="text-text-secondary">
              {" "}
              — {photo.make} {photo.model} ({photo.fileType})
            </span>
          ) : (
            <span className="text-text-secondary"> — {t("contribute.report.noPhoto")}</span>
          )}
        </span>
      </label>
      {photo && (
        <div className="flex flex-col gap-1">
          <label className="flex items-start gap-2">
            <input
              type="checkbox"
              checked={includePhotoFile}
              onChange={(e) => setIncludePhotoFile(e.target.checked)}
            />
            <span>{t("contribute.report.photoFile", { type: photo.fileType })}</span>
          </label>
          {includePhotoFile && (
            <Text variant={TextVariants.small} color="error" className="ml-6" role="note">
              {t("contribute.report.photoFileWarning")}
            </Text>
          )}
        </div>
      )}
      <div className="flex flex-col gap-1">
        <label className="flex items-start gap-2">
          <input
            type="checkbox"
            checked={includeLog && Boolean(logTail.trim())}
            disabled={!context?.logTail}
            onChange={(e) => setIncludeLog(e.target.checked)}
          />
          <span>{t("contribute.report.log")}</span>
        </label>
        {includeLog && context?.logTail && (
          <details className="ml-6">
            <summary className="cursor-pointer text-sm text-text-secondary">{t("contribute.report.logEdit")}</summary>
            <textarea
              aria-label={t("contribute.report.log")}
              className="mt-1 h-40 w-full rounded bg-bg-primary p-2 font-mono text-xs text-text-primary"
              maxLength={DIAGNOSTICS_CAP}
              onChange={(e) => setLogTail(e.target.value)}
              value={logTail}
            />
          </details>
        )}
      </div>
      {context?.screenshotAvailable && (
        <div className="flex flex-col gap-1">
          <label className="flex items-start gap-2">
            <input
              type="checkbox"
              checked={includeScreenshot && screenshot !== null}
              disabled={!screenshot}
              onChange={(e) => setIncludeScreenshot(e.target.checked)}
            />
            <span>
              {t("contribute.report.screenshot")}
              {screenshot && (
                <span className="text-text-secondary"> — {formatFileSize(base64Bytes(screenshot.data))}</span>
              )}
            </span>
          </label>
          {includeScreenshot && screenshot && (
            <img
              alt={t("contribute.report.screenshot")}
              className="ml-6 max-h-40 self-start rounded border border-surface"
              src={`data:${screenshot.mime};base64,${screenshot.data}`}
            />
          )}
        </div>
      )}
      {pendingCrash && onReviewCrash && (
        <div className="rounded bg-bg-primary p-3 text-sm">
          {t("contribute.report.pendingCrash", { date: pendingCrash.crashedOn })}{" "}
          <button type="button" className="underline" onClick={onReviewCrash}>
            {t("contribute.report.reviewCrash")}
          </button>
        </div>
      )}
    </ReportDialog>
  );
}
