// SPDX-License-Identifier: AGPL-3.0-or-later
// Where "Report a problem" (Help menu, the bar's "…" menu) and the crash
// question at start live (ADR 0017, channels 1 and 2). The crash question
// is asked once per crash; Not now keeps the record (it can still be sent
// from "Report a problem" until it expires after 30 days), Never deletes
// all records and stops keeping them (Preferences can turn it back on).
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { useModalFocus } from "../../hooks/useModalFocus";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useUIStore } from "../../store/useUIStore";
import Button from "../ui/Button";
import Text from "../ui/Text";
import { TextVariants } from "../../types/typography";
import CrashReportDialog from "./CrashReportDialog";
import ProblemReportDialog from "./ProblemReportDialog";
import {
  clearCrashReports,
  markCrashReportAsked,
  pendingCrashReports,
  photoInfo,
  problemReportContext,
  problemReportScreenshot,
  removeCrashReport,
  type CrashRecord,
  type ReportContext,
  type Screenshot,
} from "./reports";

/** The native Help menu's "Report a Problem…" (src-tauri/src/app_quit.rs). */
export const REPORT_PROBLEM_EVENT = "report-problem-requested";

/** Lets a menu that was just clicked fade out (0.1 s) before the window is
 * captured. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 250));

function CrashQuestion({
  record,
  onReview,
  onNotNow,
  onNever,
}: {
  record: CrashRecord;
  onReview(): void;
  onNotNow(): void;
  onNever(): void;
}) {
  const { t } = useTranslation();
  const panel = useRef<HTMLDivElement>(null);
  useModalFocus(panel, true);
  const content = (
    <div
      aria-labelledby="crash-question-title"
      aria-modal="true"
      className="fixed inset-0 flex items-center justify-center z-[9999] bg-black/30 backdrop-blur-xs"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          onNotNow();
        }
      }}
      role="dialog"
    >
      <div
        className="bg-surface rounded-lg shadow-xl p-6 w-full max-w-md flex flex-col gap-3 text-text-primary"
        ref={panel}
        tabIndex={-1}
      >
        <Text variant={TextVariants.title} id="crash-question-title">
          {t("contribute.crash.question")}
        </Text>
        <Text color="secondary">{t("contribute.crash.questionDetail", { date: record.crashedOn })}</Text>
        <div className="flex flex-wrap justify-end gap-3 mt-2">
          <Button variant="ghost" onClick={onNever}>
            {t("contribute.crash.never")}
          </Button>
          <Button variant="ghost" onClick={onNotNow} data-autofocus>
            {t("contribute.crash.notNow")}
          </Button>
          <Button onClick={onReview}>{t("contribute.crash.review")}</Button>
        </div>
      </div>
    </div>
  );
  return createPortal(content, document.body);
}

export default function ContributionHost() {
  const isOpen = useUIStore((s) => s.isProblemReportOpen);
  const setOpen = useUIStore((s) => s.setProblemReportOpen);
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const settingsLoaded = useSettingsStore((s) => s.appSettings !== null);
  const [context, setContext] = useState<ReportContext | null>(null);
  const [screenshot, setScreenshot] = useState<Screenshot | null>(null);
  const [shown, setShown] = useState(false);
  const [crashes, setCrashes] = useState<CrashRecord[]>([]);
  const [question, setQuestion] = useState<CrashRecord | null>(null);
  const [reviewing, setReviewing] = useState<CrashRecord | null>(null);
  const asked = useRef(false);

  // The crash question, once per start and once per crash.
  useEffect(() => {
    if (!settingsLoaded || asked.current) return;
    asked.current = true;
    if (useSettingsStore.getState().appSettings?.crashReports === false) return;
    pendingCrashReports()
      .then((records) => {
        setCrashes(records);
        const fresh = records.find((r) => !r.asked);
        if (fresh) {
          setQuestion(fresh);
          void markCrashReportAsked(fresh.id).catch(console.error);
        }
      })
      .catch(console.error);
  }, [settingsLoaded]);

  useEffect(() => {
    const unlisten = listen(REPORT_PROBLEM_EVENT, () => setOpen(true));
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, [setOpen]);

  // Opening: the window is captured as it is (after the menu closed), the
  // log read; both stay in memory and go only into a report the user sends.
  useEffect(() => {
    if (!isOpen) {
      setShown(false);
      setContext(null);
      setScreenshot(null);
      return;
    }
    let live = true;
    void (async () => {
      await settle();
      const [ctx, shot, records] = await Promise.all([
        problemReportContext().catch((e) => {
          console.error(e);
          return null;
        }),
        problemReportScreenshot().catch((e) => {
          console.error(e);
          return null;
        }),
        pendingCrashReports().catch(() => [] as CrashRecord[]),
      ]);
      if (!live) return;
      setContext(ctx);
      setScreenshot(shot);
      setCrashes(records);
      setShown(true);
    })();
    return () => {
      live = false;
    };
  }, [isOpen]);

  const forget = useCallback((id: string) => {
    void removeCrashReport(id).catch(console.error);
    setCrashes((records) => records.filter((r) => r.id !== id));
  }, []);

  const never = async () => {
    setQuestion(null);
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (appSettings) await handleSettingsChange({ ...appSettings, crashReports: false });
    await clearCrashReports().catch(console.error);
    setCrashes([]);
  };

  return (
    <>
      {question && (
        <CrashQuestion
          record={question}
          onNever={() => void never()}
          onNotNow={() => setQuestion(null)}
          onReview={() => {
            setReviewing(question);
            setQuestion(null);
          }}
        />
      )}
      <ProblemReportDialog
        isOpen={isOpen && shown}
        onClose={() => setOpen(false)}
        context={context}
        screenshot={screenshot}
        photo={photoInfo(selectedImage)}
        pendingCrash={crashes[0] ?? null}
        onReviewCrash={() => {
          setReviewing(crashes[0] ?? null);
          setOpen(false);
        }}
      />
      <CrashReportDialog
        record={reviewing}
        onClose={() => setReviewing(null)}
        onSent={() => reviewing && forget(reviewing.id)}
      />
    </>
  );
}
