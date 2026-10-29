// SPDX-License-Identifier: AGPL-3.0-or-later
// "Review what will be sent" (ADR 0017): every contribution channel shows
// its exact body here, and only this dialog's Send button sends it.
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, LoaderCircle } from "lucide-react";
import { useModalFocus } from "../../hooks/useModalFocus";
import Button from "../ui/Button";
import Text from "../ui/Text";
import { TextVariants } from "../../types/typography";
import { formatFileSize } from "../../utils/formatFileSize";
import {
  attachmentsView,
  deleteContribution,
  sendContribution,
  type ContributionError,
  type ContributionReceipt,
  type PreparedContribution,
} from "./relay";

interface ReviewSendDialogProps {
  isOpen: boolean;
  /** What is being sent, e.g. "Report a problem". */
  title: string;
  prepared: PreparedContribution | null;
  onClose(): void;
  /** Called with the relay's answer after a successful send. */
  onSent?(receipt: ContributionReceipt): void;
  /** Sends the reviewed body (tests replace it). */
  send?(prepared: PreparedContribution): Promise<ContributionReceipt>;
  /** Back to the form that made the body, to change or remove parts. */
  onBack?(): void;
  /** Deletes a sent submission by its id (tests replace it). */
  remove?(id: string): Promise<void>;
}

type Phase =
  | { kind: "review" }
  | { kind: "sending" }
  | { kind: "sent"; receipt: ContributionReceipt; deletion?: "deleting" | "deleted" | "failed" }
  | { kind: "failed"; code: ContributionError | "unknown" };

/** Where people can ask for a submission to be deleted (PRIVACY.md). */
export const CONTACT = "hello@rawtonic.cc";

const ERROR_CODES: ContributionError[] = ["not-configured", "unapproved", "rate-limited", "not-found", "refused", "network"];

export default function ReviewSendDialog({
  isOpen,
  title,
  prepared,
  onClose,
  onSent,
  send = sendContribution,
  onBack,
  remove = deleteContribution,
}: ReviewSendDialogProps) {
  const { t } = useTranslation();
  const [phase, setPhase] = useState<Phase>({ kind: "review" });
  const [copied, setCopied] = useState(false);
  const panel = useRef<HTMLDivElement>(null);
  // A real modal: focus inside, Tab cycles in it, the rest is inert, and the
  // focus returns to the opener on close.
  useModalFocus(panel, isOpen && prepared !== null);

  useEffect(() => {
    if (isOpen) {
      setPhase({ kind: "review" });
      setCopied(false);
    }
  }, [isOpen, prepared]);

  const doSend = useCallback(async () => {
    if (!prepared || phase.kind === "sending") return;
    setPhase({ kind: "sending" });
    try {
      const receipt = await send(prepared);
      setPhase({ kind: "sent", receipt });
      onSent?.(receipt);
    } catch (error) {
      const code = ERROR_CODES.find((c) => c === error) ?? "unknown";
      setPhase({ kind: "failed", code });
    }
  }, [onSent, phase.kind, prepared, send]);

  const close = useCallback(() => {
    if (phase.kind !== "sending") onClose();
  }, [onClose, phase.kind]);

  if (!isOpen || !prepared) return null;

  const errorText = (code: ContributionError | "unknown") => {
    switch (code) {
      case "not-configured":
        return t("contribute.review.errors.notConfigured");
      case "rate-limited":
        return t("contribute.review.errors.rateLimited");
      case "network":
        return t("contribute.review.errors.network");
      case "unapproved":
        return t("contribute.review.errors.unapproved");
      default:
        return t("contribute.review.errors.refused");
    }
  };

  const view = attachmentsView(prepared);
  const size = new TextEncoder().encode(prepared.body).length;
  const id = phase.kind === "sent" ? phase.receipt.id : null;
  const notUploaded = phase.kind === "sent" && (phase.receipt.notUploaded?.length ?? 0) > 0;

  const deleteNow = async () => {
    if (!id || phase.kind !== "sent" || phase.deletion === "deleting") return;
    setPhase({ ...phase, deletion: "deleting" });
    try {
      await remove(id);
      setPhase({ ...phase, deletion: "deleted" });
    } catch {
      setPhase({ ...phase, deletion: "failed" });
    }
  };

  const copyId = async () => {
    if (!id) return;
    try {
      await navigator.clipboard.writeText(id);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };

  const content = (
    <div
      aria-labelledby="review-send-title"
      aria-modal="true"
      className="studio-dialog-backdrop"
      onClick={close}
      onKeyDown={(e) => {
        // Enter never sends: only a click on Send (or Space/Enter on the
        // focused button itself) does.
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          close();
        }
      }}
      role="dialog"
    >
      <div
        className="studio-dialog is-wide"
        onClick={(e) => e.stopPropagation()}
        ref={panel}
        tabIndex={-1}
      >
        <h2 className="studio-dialog-title" id="review-send-title">
          {title}
        </h2>
        {phase.kind === "sent" ? (
          <div className="flex flex-col gap-3">
            <Text>{t("contribute.review.sent")}</Text>
            {notUploaded && (
              <Text color="error" role="alert">
                {t("contribute.review.notUploaded")}
              </Text>
            )}
            {id && (
              <>
                <div className="flex items-center gap-2">
                  <code className="select-all rounded bg-[#1f2022] px-2 py-1 font-mono text-sm" data-testid="report-id">
                    {id}
                  </code>
                  <Button variant="ghost" onClick={() => void copyId()} aria-label={t("contribute.review.copyId")}>
                    {copied ? <Check size={14} /> : <ClipboardCopy size={14} />}
                  </Button>
                </div>
                <Text color="secondary">{t("contribute.review.idHint")}</Text>
                <Text color="secondary">{t("contribute.review.deleteHint", { email: CONTACT })}</Text>
                <div className="flex items-center gap-3">
                  <Button
                    variant="ghost"
                    onClick={() => void deleteNow()}
                    disabled={phase.deletion === "deleting" || phase.deletion === "deleted"}
                  >
                    {t("contribute.review.deleteNow")}
                  </Button>
                  {phase.deletion === "deleted" && (
                    <Text role="status">{t("contribute.review.deleted")}</Text>
                  )}
                  {phase.deletion === "failed" && (
                    <Text color="error" role="alert">
                      {t("contribute.review.deleteFailed", { email: CONTACT })}
                    </Text>
                  )}
                </div>
              </>
            )}
          </div>
        ) : (
          <>
            <Text color="secondary">
              {t("contribute.review.intro")}
            </Text>
            <pre
              aria-label={t("contribute.review.bodyLabel")}
              className="min-h-0 flex-1 overflow-auto p-3 font-mono text-xs whitespace-pre-wrap wrap-anywhere"
              tabIndex={0}
            >
              {prepared.body}
            </pre>
            {view.screenshot && (
              <figure>
                <img
                  alt={t("contribute.review.screenshot")}
                  className="max-h-48 rounded border border-[#3c3f43]"
                  src={view.screenshot.src}
                />
                <figcaption className="text-text-secondary text-xs mt-1">
                  {t("contribute.review.screenshotSize", { size: formatFileSize(view.screenshot.bytes) })}
                </figcaption>
              </figure>
            )}
            {view.photo && (
              <Text variant={TextVariants.small} color="secondary">
                {t("contribute.review.photoFile", { size: formatFileSize(view.photo.bytes) })}
              </Text>
            )}
            <Text variant={TextVariants.small} color="secondary">
              {t("contribute.review.size", { size: formatFileSize(size) })}
            </Text>
            {phase.kind === "failed" && (
              <Text color="error" role="alert">
                {errorText(phase.code)}
              </Text>
            )}
          </>
        )}
        <div className="studio-dialog-footer">
          {phase.kind === "sent" ? (
            <Button onClick={onClose} data-autofocus>
              {t("contribute.review.close")}
            </Button>
          ) : (
            <>
              {onBack && (
                <Button variant="ghost" onClick={onBack} disabled={phase.kind === "sending"}>
                  {t("contribute.review.change")}
                </Button>
              )}
              <div className="studio-dialog-footer-end">
                <Button variant="ghost" onClick={close} disabled={phase.kind === "sending"} data-autofocus>
                  {t("contribute.review.cancel")}
                </Button>
                <Button onClick={() => void doSend()} disabled={phase.kind === "sending"}>
                  {phase.kind === "sending" ? (
                    <>
                      <LoaderCircle size={14} className="animate-spin" />
                      {t("contribute.review.sending")}
                    </>
                  ) : (
                    t("contribute.review.send")
                  )}
                </Button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );

  return typeof document !== "undefined" ? createPortal(content, document.body) : content;
}
