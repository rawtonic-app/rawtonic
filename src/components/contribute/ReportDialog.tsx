// SPDX-License-Identifier: AGPL-3.0-or-later
// The frame of the problem and crash report forms (ADR 0017): the form,
// then "Review and send…" into ReviewSendDialog, which shows the exact body;
// "Copy report" and "Open on GitHub" work without the relay. Every way out
// uses the body the app prepared (scrubbed), never the raw input.
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-shell";
import { Check, ClipboardCopy, ExternalLink } from "lucide-react";
import { useModalFocus } from "../../hooks/useModalFocus";
import Button from "../ui/Button";
import Text from "../ui/Text";
import { TextVariants } from "../../types/typography";
import ReviewSendDialog from "./ReviewSendDialog";
import {
  contributionAvailable,
  prepareContribution,
  type ContributionChannel,
  type ContributionReceipt,
  type PreparedContribution,
} from "./relay";
import { githubIssue, reportMarkdown, textOnly, withFileLeftOut, type GithubIssue } from "./reports";

interface ReportDialogProps {
  isOpen: boolean;
  title: string;
  channel: ContributionChannel;
  /** The payload for the form's current choices, or null while it is not
   * complete (the actions are disabled then). */
  payload: Record<string, unknown> | null;
  children: ReactNode;
  onClose(): void;
  onSent?(receipt: ContributionReceipt): void;
  /** Tests replace the app's calls. */
  prepare?(channel: ContributionChannel, payload: Record<string, unknown>): Promise<PreparedContribution>;
  available?(): Promise<boolean>;
  openUrl?(url: string): Promise<void>;
  send?(prepared: PreparedContribution): Promise<ContributionReceipt>;
}

export default function ReportDialog({
  isOpen,
  title,
  channel,
  payload,
  children,
  onClose,
  onSent,
  prepare = prepareContribution,
  available = contributionAvailable,
  openUrl = open,
  send,
}: ReportDialogProps) {
  const { t } = useTranslation();
  const [relay, setRelay] = useState<boolean | null>(null);
  const [prepared, setPrepared] = useState<PreparedContribution | null>(null);
  const [copied, setCopied] = useState(false);
  // "Open on GitHub" shows the exact title and text its link carries first:
  // the browser sends them to GitHub when it loads the form.
  const [github, setGithub] = useState<GithubIssue | null>(null);
  const [error, setError] = useState<string | null>(null);
  const panel = useRef<HTMLDivElement>(null);
  // A preparation answers for the form as it was asked: one that arrives
  // after the form changed, closed or opened again is dropped.
  const generation = useRef(0);
  useModalFocus(panel, isOpen && prepared === null);

  useEffect(() => {
    if (!isOpen) return;
    setPrepared(null);
    setGithub(null);
    setCopied(false);
    setError(null);
    let live = true;
    available()
      .then((yes) => live && setRelay(yes))
      .catch(() => live && setRelay(false));
    return () => {
      live = false;
    };
  }, [available, isOpen]);

  useEffect(() => {
    generation.current += 1;
    setCopied(false);
  }, [payload, isOpen]);

  /** `text`: for Copy report / Open on GitHub, which carry no files. */
  const scrubbed = useCallback(async (text = false) => {
    if (!payload) return null;
    const asked = generation.current;
    setError(null);
    try {
      const body = await prepare(channel, text ? textOnly(payload) : payload);
      return generation.current === asked ? body : null;
    } catch {
      if (generation.current === asked) setError(t("contribute.report.prepareFailed"));
      return null;
    }
  }, [channel, payload, prepare, t]);

  const review = async () => {
    const body = await scrubbed();
    if (body) setPrepared(body);
  };

  // WebKit takes clipboard writes only within the click: the write starts
  // at once with the text still being prepared (a promised ClipboardItem).
  const copy = async () => {
    const text = scrubbed(true).then((body) => {
      if (!body || !payload) throw new Error("not prepared");
      return reportMarkdown(withFileLeftOut(body.body, payload));
    });
    try {
      if (typeof ClipboardItem !== "undefined" && navigator.clipboard.write) {
        await navigator.clipboard.write([
          new ClipboardItem({ "text/plain": text.then((t) => new Blob([t], { type: "text/plain" })) }),
        ]);
      } else {
        await navigator.clipboard.writeText(await text);
      }
      setCopied(true);
    } catch {
      // A failed preparation already said why.
      if (await text.then(() => true, () => false)) setError(t("contribute.report.copyFailed"));
    }
  };

  const reviewGithub = async () => {
    const body = await scrubbed(true);
    if (body && payload) setGithub(githubIssue(withFileLeftOut(body.body, payload)));
  };

  const openGithub = async () => {
    if (!github) return;
    try {
      await openUrl(github.url);
      onClose();
    } catch {
      setError(t("contribute.report.openFailed"));
    }
  };

  if (!isOpen) return null;

  if (prepared) {
    return (
      <ReviewSendDialog
        isOpen
        title={title}
        prepared={prepared}
        onClose={onClose}
        onBack={() => setPrepared(null)}
        onSent={onSent}
        {...(send ? { send } : {})}
      />
    );
  }

  const content = (
    <div
      aria-labelledby="report-dialog-title"
      aria-modal="true"
      className="studio-dialog-backdrop"
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
        className="studio-dialog is-wide"
        ref={panel}
        tabIndex={-1}
      >
        <h2 className="studio-dialog-title" id="report-dialog-title">
          {title}
        </h2>
        {github ? (
          <>
            <Text color="secondary">
              {t("contribute.report.githubReview")}
            </Text>
            <pre
              aria-label={t("contribute.report.githubText")}
              className="min-h-0 flex-1 overflow-auto p-3 font-mono text-xs whitespace-pre-wrap wrap-anywhere"
              tabIndex={0}
            >
              {`${github.title}\n\n${github.body}`}
            </pre>
            {error && (
              <Text color="error" role="alert">
                {error}
              </Text>
            )}
            <div className="studio-dialog-footer">
              <Button variant="ghost" onClick={() => setGithub(null)}>
                {t("contribute.review.change")}
              </Button>
              <div className="studio-dialog-footer-end">
                <Button variant="ghost" onClick={onClose} data-autofocus>
                  {t("contribute.review.cancel")}
                </Button>
                <Button onClick={() => void openGithub()}>{t("contribute.report.githubOpen")}</Button>
              </div>
            </div>
          </>
        ) : (
          <>
            <div className="min-h-0 flex-1 overflow-auto flex flex-col gap-3 pr-1">{children}</div>
            {relay === false && (
              <Text variant={TextVariants.small} color="secondary">
                {t("contribute.report.noRelay")}
              </Text>
            )}
            <Text variant={TextVariants.small} color="secondary">
              {t("contribute.report.githubPublic")}
            </Text>
            {error && (
              <Text color="error" role="alert">
                {error}
              </Text>
            )}
            <div className="studio-dialog-footer">
              <Button variant="ghost" onClick={() => void copy()} disabled={!payload}>
                {copied ? <Check size={14} /> : <ClipboardCopy size={14} />}
                {copied ? t("contribute.report.copied") : t("contribute.report.copy")}
              </Button>
              <Button variant="ghost" onClick={() => void reviewGithub()} disabled={!payload}>
                <ExternalLink size={14} />
                {t("contribute.report.github")}
              </Button>
              <div className="studio-dialog-footer-end">
                <Button variant="ghost" onClick={onClose} data-autofocus>
                  {t("contribute.review.cancel")}
                </Button>
                {relay && (
                  <Button onClick={() => void review()} disabled={!payload}>
                    {t("contribute.report.review")}
                  </Button>
                )}
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );

  return typeof document !== "undefined" ? createPortal(content, document.body) : content;
}
