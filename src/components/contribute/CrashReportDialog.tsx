// SPDX-License-Identifier: AGPL-3.0-or-later
// The crash report of an earlier run (ADR 0017, channel 2): the allowlisted
// fields the app recorded, with the backtrace removable and the log's last
// lines editable or removable, then the same review as every channel.
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import Text from "../ui/Text";
import { TextVariants } from "../../types/typography";
import ReportDialog from "./ReportDialog";
import { DIAGNOSTICS_CAP, crashPayload, type CrashRecord } from "./reports";
import type { ContributionChannel, ContributionReceipt, PreparedContribution } from "./relay";

interface CrashReportDialogProps {
  record: CrashRecord | null;
  onClose(): void;
  onSent?(receipt: ContributionReceipt): void;
  /** Tests replace the app's calls. */
  prepare?(channel: ContributionChannel, payload: Record<string, unknown>): Promise<PreparedContribution>;
  available?(): Promise<boolean>;
  openUrl?(url: string): Promise<void>;
  send?(prepared: PreparedContribution): Promise<ContributionReceipt>;
}

export default function CrashReportDialog({ record, onClose, onSent, ...calls }: CrashReportDialogProps) {
  const { t } = useTranslation();
  const [includeBacktrace, setIncludeBacktrace] = useState(true);
  const [includeLog, setIncludeLog] = useState(true);
  const [logTail, setLogTail] = useState("");
  const [message, setMessage] = useState("");

  useEffect(() => {
    setIncludeBacktrace(true);
    setIncludeLog(true);
    setLogTail(record?.logTail ?? "");
    setMessage(record?.message ?? "");
  }, [record]);

  const payload = useMemo(
    () =>
      record ? crashPayload(record, { message, includeBacktrace, logTail: includeLog ? logTail : null }) : null,
    [includeBacktrace, includeLog, logTail, message, record],
  );

  return (
    <ReportDialog
      isOpen={record !== null}
      title={t("contribute.crash.title")}
      channel="crash"
      payload={payload}
      onClose={onClose}
      onSent={onSent}
      {...calls}
    >
      {record && (
        <>
          <Text>{t("contribute.crash.intro", { date: record.crashedOn })}</Text>
          <code className="rounded bg-bg-primary p-2 font-mono text-xs">{record.location}</code>
          <label className="flex flex-col gap-1">
            <Text variant={TextVariants.small} color="secondary">
              {t("contribute.crash.messageLabel")}
            </Text>
            <textarea
              className="h-20 rounded bg-bg-primary p-2 font-mono text-xs text-text-primary"
              maxLength={2000}
              onChange={(e) => setMessage(e.target.value)}
              value={message}
            />
          </label>
          <Text variant={TextVariants.small} color="secondary">
            {t("contribute.crash.always")}
          </Text>
          {record.backtrace && (
            <div className="flex flex-col gap-1">
              <label className="flex items-start gap-2">
                <input
                  type="checkbox"
                  checked={includeBacktrace}
                  onChange={(e) => setIncludeBacktrace(e.target.checked)}
                />
                <span>{t("contribute.crash.backtrace")}</span>
              </label>
              {includeBacktrace && (
                <pre className="ml-6 max-h-40 overflow-auto rounded bg-bg-primary p-2 font-mono text-xs whitespace-pre-wrap wrap-anywhere">
                  {record.backtrace}
                </pre>
              )}
            </div>
          )}
          {record.logTail && (
            <div className="flex flex-col gap-1">
              <label className="flex items-start gap-2">
                <input type="checkbox" checked={includeLog} onChange={(e) => setIncludeLog(e.target.checked)} />
                <span>{t("contribute.report.log")}</span>
              </label>
              {includeLog && (
                <textarea
                  aria-label={t("contribute.report.log")}
                  className="ml-6 h-40 rounded bg-bg-primary p-2 font-mono text-xs text-text-primary"
                  maxLength={DIAGNOSTICS_CAP}
                  onChange={(e) => setLogTail(e.target.value)}
                  value={logTail}
                />
              )}
            </div>
          )}
        </>
      )}
    </ReportDialog>
  );
}
