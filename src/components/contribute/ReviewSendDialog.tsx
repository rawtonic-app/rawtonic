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
}

type Phase =
  | { kind: "review" }
  | { kind: "sending" }
  | { kind: "sent"; receipt: ContributionReceipt }
  | { kind: "failed"; code: ContributionError | "unknown" };

const ERROR_CODES: ContributionError[] = ["not-configured", "unapproved", "rate-limited", "not-found", "refused", "network"];

export default function ReviewSendDialog({
  isOpen,
  title,
  prepared,
  onClose,
  onSent,
  send = sendContribution,
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
      className="fixed inset-0 flex items-center justify-center z-[9999] bg-black/30 backdrop-blur-xs"
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
        className="bg-surface rounded-lg shadow-xl p-6 w-full max-w-2xl max-h-[85vh] flex flex-col text-text-primary"
        onClick={(e) => e.stopPropagation()}
        ref={panel}
        tabIndex={-1}
      >
        <Text variant={TextVariants.title} id="review-send-title" className="mb-2">
          {title}
        </Text>
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
                  <code className="select-all rounded bg-bg-primary px-2 py-1 font-mono text-sm" data-testid="report-id">
                    {id}
                  </code>
                  <Button variant="ghost" onClick={() => void copyId()} aria-label={t("contribute.review.copyId")}>
                    {copied ? <Check size={16} /> : <ClipboardCopy size={16} />}
                  </Button>
                </div>
                <Text color="secondary">{t("contribute.review.idHint")}</Text>
              </>
            )}
          </div>
        ) : (
          <>
            <Text color="secondary" className="mb-3">
              {t("contribute.review.intro")}
            </Text>
            <pre
              aria-label={t("contribute.review.bodyLabel")}
              className="min-h-0 flex-1 overflow-auto rounded bg-bg-primary p-3 font-mono text-xs whitespace-pre-wrap wrap-anywhere"
              tabIndex={0}
            >
              {prepared.body}
            </pre>
            {view.screenshot && (
              <figure className="mt-3">
                <img
                  alt={t("contribute.review.screenshot")}
                  className="max-h-48 rounded border border-surface"
                  src={view.screenshot.src}
                />
                <figcaption className="text-text-secondary text-xs mt-1">
                  {t("contribute.review.screenshotSize", { size: formatFileSize(view.screenshot.bytes) })}
                </figcaption>
              </figure>
            )}
            {view.photo && (
              <Text variant={TextVariants.small} color="secondary" className="mt-2">
                {t("contribute.review.photoFile", { size: formatFileSize(view.photo.bytes) })}
              </Text>
            )}
            <Text variant={TextVariants.small} color="secondary" className="mt-2">
              {t("contribute.review.size", { size: formatFileSize(size) })}
            </Text>
            {phase.kind === "failed" && (
              <Text color="error" className="mt-2" role="alert">
                {errorText(phase.code)}
              </Text>
            )}
          </>
        )}
        <div className="flex justify-end gap-3 mt-5">
          {phase.kind === "sent" ? (
            <Button onClick={onClose} data-autofocus>
              {t("contribute.review.close")}
            </Button>
          ) : (
            <>
              <Button
                className="bg-bg-primary shadow-transparent hover:bg-bg-primary text-white shadow-none"
                variant="ghost"
                onClick={close}
                disabled={phase.kind === "sending"}
                data-autofocus
              >
                {t("contribute.review.cancel")}
              </Button>
              <Button onClick={() => void doSend()} disabled={phase.kind === "sending"}>
                {phase.kind === "sending" ? (
                  <span className="flex items-center gap-2">
                    <LoaderCircle size={16} className="animate-spin" />
                    {t("contribute.review.sending")}
                  </span>
                ) : (
                  t("contribute.review.send")
                )}
              </Button>
            </>
          )}
        </div>
      </div>
    </div>
  );

  return typeof document !== "undefined" ? createPortal(content, document.body) : content;
}
