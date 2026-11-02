// SPDX-License-Identifier: AGPL-3.0-or-later
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Invokes } from "../ui/AppProperties";
import { registerAppCommands } from "../../utils/appCommands";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useProcessStore } from "../../store/useProcessStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useStackStore } from "../../store/useStackStore";
import { currentEditForExport, useEditorStore } from "../../store/useEditorStore";
import { computeSortedLibrary } from "../../hooks/useSortedLibrary";
import { flushRecipeSaves } from "../../hooks/useEditorActions";
import { photoWritesAndStepsSettled } from "../../utils/photoWrites";
import { readFormDraft, readJobName, startFormDraft, writeFormDraft, writeJobName } from "../../utils/exportJob";
import {
  exportSettingsFromDraft,
  watermarkReady,
  type RecipeDraft,
} from "../../utils/exportRecipes";
import { submittedFrom } from "../../utils/runExportJobs";
import { usableSessionFolder } from "../../utils/sessionFolders";
import { renderErrorMessage } from "../../utils/renderErrors";
import {
  CAPTION_PRESETS,
  PAPERS,
  SHARPEN_AMOUNTS,
  readProofDraft,
  sheetFromDraft,
  sheetPhotos,
  sheetSharpening,
  useProofSheetDialog,
  writeProofDraft,
  type Paper,
  type SharpenLevel,
  type ProofDraft,
} from "../../utils/proofSheet";

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** The first page as the backend lays it out (`proof_sheet_preview`). */
interface Preview {
  layout: {
    pageWidthMm: number;
    pageHeightMm: number;
    pages: { cells: { photo: number; cell: Rect; photoBox: Rect; captionBaseline: number | null }[] }[];
    boxPx: [number, number];
    headerBaseline: number | null;
    footerBaseline: number | null;
  };
  cropMarks: [number, number, number, number][];
  captions: string[];
  header: string | null;
  footer: string | null;
  fileName: string;
}

/** The photos shown in the grid, in its order. */
function shownPhotos(): string[] {
  const library = useLibraryStore.getState();
  return computeSortedLibrary(
    { ...library, stacks: useStackStore.getState().byPath, stackView: library.activeAlbumId === null },
    useSettingsStore.getState(),
  ).map((image) => image.path);
}

/** Opens the Proof Sheet dialog for the selection, else every photo shown. */
export function showProofSheet(paths?: string[]) {
  const shown = shownPhotos();
  const photos = paths ? sheetPhotos(shown, paths) : sheetPhotos(shown, useLibraryStore.getState().multiSelectedPaths);
  if (photos.length) useProofSheetDialog.getState().show(photos);
}

/** Registers File › Proof Sheet… and shows the dialog when asked. */
export function ProofSheetDialogHost() {
  const paths = useProofSheetDialog((s) => s.paths);
  const { t } = useTranslation();
  useEffect(
    () =>
      registerAppCommands({
        proof_sheet: {
          enabled: () => useLibraryStore.getState().imageList.length > 0,
          disabledHint: () => t("proofSheet.noPhotos"),
          run: () => showProofSheet(),
        },
      }),
    [t],
  );
  if (!paths) return null;
  return <ProofSheetDialog paths={paths} />;
}

const PT_TO_MM = 25.4 / 72;
const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

function ProofSheetDialog({ paths }: { paths: string[] }) {
  const { t } = useTranslation();
  const close = useProofSheetDialog((s) => s.close);
  const session = useLibraryStore((s) => s.sessionDirectory);
  const thumbnails = useProcessStore((s) => s.thumbnails);
  const [draft, setDraft] = useState<ProofDraft>(() => readProofDraft(session));
  const [jobName, setJobName] = useState(() => readJobName(session));
  const [preview, setPreview] = useState<Preview | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const dialog = useRef<HTMLDivElement>(null);
  const set = (patch: Partial<ProofDraft>) => setDraft((current) => ({ ...current, ...patch }));

  // The watermark is the one set in the Export form of this library. When
  // that form has none yet, its text is typed here and kept in the form on
  // Create, without switching the watermark on for exports.
  const exportForm = useMemo(() => readFormDraft(session), [session]);
  const formReady = !!exportForm && watermarkReady(exportForm);
  const [watermarkText, setWatermarkText] = useState("");
  const watermarkForm: RecipeDraft | null = formReady
    ? exportForm
    : watermarkText.trim()
      ? { ...(exportForm ?? startFormDraft(session)), watermarkKind: "text", watermarkText }
      : null;
  const watermark = watermarkForm
    ? exportSettingsFromDraft({ ...watermarkForm, watermark: true }).settings.watermark
    : null;
  const target = draft.where === "output" && !session ? "choose" : draft.where;

  const request = (destination: string) => ({
    paths,
    sheet: sheetFromDraft(draft),
    destination,
    name: draft.name,
    jobName,
    watermark: draft.watermark ? watermark : null,
    outputSharpening: sheetSharpening(draft),
    ...currentEditForExport(useEditorStore.getState()),
    library: submittedFrom(session),
  });

  useEffect(() => {
    dialog.current?.focus();
  }, []);

  // The first page, laid out by the backend as it will be written.
  useEffect(() => {
    let current = true;
    const timer = window.setTimeout(() => {
      invoke<Preview>(Invokes.ProofSheetPreview, { request: request("") })
        .then((next) => {
          if (!current) return;
          setPreview(next);
          setProblem(null);
        })
        .catch((e) => {
          if (current) setProblem(renderErrorMessage(e));
        });
    }, 150);
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
    // The request is made from these.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft, jobName, paths, session]);

  const create = async () => {
    setBusy(true);
    setProblem(null);
    try {
      const unsaved = (await Promise.all([flushRecipeSaves(), photoWritesAndStepsSettled()]))[0].filter((p) =>
        paths.includes(p),
      );
      if (unsaved.length)
        throw new Error(t("studio.export.toasts.unsavedEdits", { names: unsaved.map(fileName).join(", ") }));
      let destination: string | null = null;
      if (target === "output" && session) {
        destination = await usableSessionFolder(session, "output");
      } else if (draft.folder) {
        destination = draft.folder;
      } else {
        const chosen = await open({ title: t("proofSheet.chooseFolder"), directory: true, multiple: false });
        destination = typeof chosen === "string" ? chosen : null;
      }
      if (!destination) return;
      await invoke(Invokes.QueueProofSheet, { request: request(destination) });
      writeProofDraft(session, draft);
      writeJobName(session, jobName);
      if (draft.watermark && !formReady && watermarkForm) writeFormDraft(session, watermarkForm);
      close();
    } catch (e) {
      setProblem(renderErrorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const chooseFolder = async () => {
    const chosen = await open({
      title: t("proofSheet.chooseFolder"),
      directory: true,
      multiple: false,
      defaultPath: draft.folder,
    });
    if (typeof chosen === "string") set({ where: "choose", folder: chosen });
  };

  const keys = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
    }
  };

  const number = (value: string) => (value.trim() === "" ? Number.NaN : Number(value));
  const page = preview?.layout;
  const pages = page?.pages.length ?? 0;
  const cells = page?.pages[0]?.cells ?? [];
  // The preview page fits a 300 × 400 box; text sizes scale with it.
  const scale = page ? Math.min(300 / page.pageWidthMm, 400 / page.pageHeightMm) : 1;
  const mm = (value: number) => `${value * scale}px`;
  const [, rightMargin, , leftMargin] = sheetFromDraft(draft).marginsMm;
  const title = t("proofSheet.title");

  return createPortal(
    <div className="studio-dialog-backdrop" onKeyDown={keys}>
      <div
        ref={dialog}
        className="studio-dialog studio-proof-sheet"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
      >
        <h2 className="studio-dialog-title">{title}</h2>
        <div className="studio-proof-sheet-body">
          <figure className="studio-proof-sheet-preview" aria-label={t("proofSheet.preview")}>
            {page && (
              <div
                className="studio-proof-sheet-page"
                style={{ width: mm(page.pageWidthMm), height: mm(page.pageHeightMm) }}
              >
                {preview?.header && page.headerBaseline !== null && (
                  <span
                    className="studio-proof-sheet-text"
                    style={{
                      left: mm(leftMargin),
                      top: mm(page.headerBaseline - draft.textPt * PT_TO_MM * 0.8),
                      fontSize: mm(draft.textPt * PT_TO_MM),
                      maxWidth: mm(page.pageWidthMm - leftMargin - rightMargin),
                    }}
                  >
                    {preview.header}
                  </span>
                )}
                {page.footerBaseline !== null && (
                  <span
                    className="studio-proof-sheet-text studio-proof-sheet-footer"
                    style={{
                      left: mm(leftMargin),
                      right: mm(rightMargin),
                      top: mm(page.footerBaseline - draft.textPt * PT_TO_MM * 0.8),
                      fontSize: mm(draft.textPt * PT_TO_MM),
                    }}
                  >
                    <span>{preview?.footer ?? ""}</span>
                    {draft.pageNumbers && <span>{`1 / ${pages}`}</span>}
                  </span>
                )}
                {preview && preview.cropMarks.length > 0 && (
                  <svg
                    className="studio-proof-sheet-marks"
                    width={mm(page.pageWidthMm)}
                    height={mm(page.pageHeightMm)}
                    aria-hidden="true"
                  >
                    {preview.cropMarks.map(([x0, y0, x1, y1], i) => (
                      <line key={i} x1={x0 * scale} y1={y0 * scale} x2={x1 * scale} y2={y1 * scale} stroke="#000" strokeWidth={1} />
                    ))}
                  </svg>
                )}
                {cells.map((cell) => {
                  const path = paths[cell.photo];
                  const thumbnail = thumbnails[path];
                  return (
                    <div key={cell.photo}>
                      <div
                        className="studio-proof-sheet-photo"
                        style={{
                          left: mm(cell.photoBox.x),
                          top: mm(cell.photoBox.y),
                          width: mm(cell.photoBox.w),
                          height: mm(cell.photoBox.h),
                        }}
                      >
                        {thumbnail ? <img src={thumbnail} alt="" /> : <span className="studio-proof-sheet-missing" />}
                      </div>
                      {cell.captionBaseline !== null && (
                        <span
                          className="studio-proof-sheet-text studio-proof-sheet-caption"
                          style={{
                            left: mm(cell.cell.x),
                            width: mm(cell.cell.w),
                            top: mm(cell.captionBaseline - draft.captionPt * PT_TO_MM * 0.8),
                            fontSize: mm(draft.captionPt * PT_TO_MM),
                          }}
                        >
                          {preview?.captions[cell.photo] ?? ""}
                        </span>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
            <figcaption className="studio-help">
              {page
                ? t("proofSheet.summary", {
                    photos: paths.length,
                    pages,
                    perPage: draft.columns * draft.rows,
                    width: page.boxPx[0],
                    height: page.boxPx[1],
                  })
                : " "}
            </figcaption>
          </figure>

          <div className="studio-proof-sheet-form">
            <fieldset>
              <legend>{t("proofSheet.page")}</legend>
              <label>
                {t("proofSheet.paper")}
                <select value={draft.paper} onChange={(e) => set({ paper: e.target.value as Paper })}>
                  {(Object.keys(PAPERS) as (keyof typeof PAPERS)[]).map((paper) => (
                    <option key={paper} value={paper}>
                      {t(`proofSheet.papers.${paper}`)}
                    </option>
                  ))}
                  <option value="custom">{t("proofSheet.papers.custom")}</option>
                </select>
              </label>
              {draft.paper === "custom" && (
                <div className="studio-proof-sheet-pair">
                  <label>
                    {t("proofSheet.widthMm")}
                    <input
                      type="number"
                      min={20}
                      max={2000}
                      value={draft.customWidthMm}
                      onChange={(e) => set({ customWidthMm: number(e.target.value) })}
                    />
                  </label>
                  <label>
                    {t("proofSheet.heightMm")}
                    <input
                      type="number"
                      min={20}
                      max={2000}
                      value={draft.customHeightMm}
                      onChange={(e) => set({ customHeightMm: number(e.target.value) })}
                    />
                  </label>
                </div>
              )}
              <div className="studio-proof-sheet-choice" role="radiogroup" aria-label={t("proofSheet.orientation")}>
                <label>
                  <input type="radio" checked={!draft.landscape} onChange={() => set({ landscape: false })} />
                  {t("proofSheet.portrait")}
                </label>
                <label>
                  <input type="radio" checked={draft.landscape} onChange={() => set({ landscape: true })} />
                  {t("proofSheet.landscape")}
                </label>
              </div>
              <label>
                {t("proofSheet.marginMm")}
                <input
                  type="number"
                  min={0}
                  max={100}
                  value={draft.marginMm}
                  onChange={(e) => set({ marginMm: number(e.target.value) })}
                />
              </label>
            </fieldset>

            <fieldset>
              <legend>{t("proofSheet.grid")}</legend>
              <div className="studio-proof-sheet-pair">
                <label>
                  {t("proofSheet.columns")}
                  <input
                    type="number"
                    min={1}
                    max={20}
                    value={draft.columns}
                    onChange={(e) => set({ columns: number(e.target.value) })}
                  />
                </label>
                <label>
                  {t("proofSheet.rows")}
                  <input
                    type="number"
                    min={1}
                    max={30}
                    value={draft.rows}
                    onChange={(e) => set({ rows: number(e.target.value) })}
                  />
                </label>
                <label>
                  {t("proofSheet.gapMm")}
                  <input
                    type="number"
                    min={0}
                    max={50}
                    value={draft.gapMm}
                    onChange={(e) => set({ gapMm: number(e.target.value) })}
                  />
                </label>
              </div>
            </fieldset>

            <fieldset>
              <legend>{t("proofSheet.text")}</legend>
              <label>
                {t("proofSheet.caption")}
                <input
                  list="proof-sheet-captions"
                  value={draft.caption}
                  placeholder={t("proofSheet.noCaption")}
                  onChange={(e) => set({ caption: e.target.value })}
                />
                <datalist id="proof-sheet-captions">
                  {CAPTION_PRESETS.filter(Boolean).map((preset) => (
                    <option key={preset} value={preset} />
                  ))}
                </datalist>
              </label>
              <p className="studio-help">{t("proofSheet.captionHelp")}</p>
              <label>
                {t("proofSheet.header")}
                <input value={draft.header} onChange={(e) => set({ header: e.target.value })} />
              </label>
              <label>
                {t("proofSheet.footer")}
                <input value={draft.footer} onChange={(e) => set({ footer: e.target.value })} />
              </label>
              <p className="studio-help">{t("proofSheet.textHelp")}</p>
              <label>
                {t("proofSheet.jobName")}
                <input value={jobName} onChange={(e) => setJobName(e.target.value)} />
              </label>
              <div className="studio-proof-sheet-pair">
                <label>
                  {t("proofSheet.captionPt")}
                  <input
                    type="number"
                    min={4}
                    max={72}
                    value={draft.captionPt}
                    onChange={(e) => set({ captionPt: number(e.target.value) })}
                  />
                </label>
                <label>
                  {t("proofSheet.textPt")}
                  <input
                    type="number"
                    min={4}
                    max={72}
                    value={draft.textPt}
                    onChange={(e) => set({ textPt: number(e.target.value) })}
                  />
                </label>
              </div>
              <label className="studio-proof-sheet-check">
                <input
                  type="checkbox"
                  checked={draft.pageNumbers}
                  onChange={(e) => set({ pageNumbers: e.target.checked })}
                />
                {t("proofSheet.pageNumbers")}
              </label>
              <label className="studio-proof-sheet-check">
                <input
                  type="checkbox"
                  checked={draft.cropMarks}
                  onChange={(e) => set({ cropMarks: e.target.checked })}
                />
                {t("proofSheet.cropMarks")}
              </label>
            </fieldset>

            <fieldset>
              <legend>{t("proofSheet.output")}</legend>
              <label className="studio-proof-sheet-check">
                <input
                  type="checkbox"
                  checked={draft.watermark}
                  onChange={(e) => set({ watermark: e.target.checked })}
                />
                {formReady ? t("proofSheet.watermark") : t("proofSheet.watermarkNew")}
              </label>
              {draft.watermark && !formReady && (
                <>
                  <label>
                    {t("proofSheet.watermarkText")}
                    <input
                      type="text"
                      value={watermarkText}
                      placeholder="©"
                      onChange={(e) => setWatermarkText(e.target.value)}
                    />
                  </label>
                  <p className="studio-help">{t("proofSheet.watermarkHelp")}</p>
                </>
              )}
              <label className="studio-proof-sheet-check">
                <input type="checkbox" checked={draft.sharpen} onChange={(e) => set({ sharpen: e.target.checked })} />
                {t("proofSheet.sharpen")}
              </label>
              {draft.sharpen && (
                <label>
                  {t("proofSheet.sharpenLevel")}
                  <select
                    value={draft.sharpenLevel}
                    onChange={(e) => set({ sharpenLevel: e.target.value as SharpenLevel })}
                  >
                    {(Object.keys(SHARPEN_AMOUNTS) as SharpenLevel[]).map((level) => (
                      <option key={level} value={level}>
                        {t(`proofSheet.sharpenLevels.${level}`)}
                      </option>
                    ))}
                  </select>
                </label>
              )}
              <label>
                {t("proofSheet.ppi")}
                <input
                  type="number"
                  min={72}
                  max={1200}
                  value={draft.ppi}
                  onChange={(e) => set({ ppi: number(e.target.value) })}
                />
              </label>
              <div className="studio-proof-sheet-choice">
                <label>
                  <input type="checkbox" checked={draft.pdf} onChange={(e) => set({ pdf: e.target.checked })} />
                  {t("proofSheet.pdf")}
                </label>
                <label>
                  <input type="checkbox" checked={draft.jpeg} onChange={(e) => set({ jpeg: e.target.checked })} />
                  {t("proofSheet.jpeg")}
                </label>
              </div>
              <label>
                {t("proofSheet.fileName")}
                <input value={draft.name} onChange={(e) => set({ name: e.target.value })} />
              </label>
              <div className="studio-proof-sheet-choice">
                {session && (
                  <label>
                    <input type="radio" checked={target === "output"} onChange={() => set({ where: "output" })} />
                    {t("proofSheet.outputFolder")}
                  </label>
                )}
                <label>
                  <input type="radio" checked={target === "choose"} onChange={() => void chooseFolder()} />
                  {target === "choose" && draft.folder ? fileName(draft.folder) : t("proofSheet.otherFolder")}
                </label>
              </div>
              {preview && <p className="studio-help">{t("proofSheet.writes", { name: preview.fileName })}</p>}
            </fieldset>
          </div>
        </div>
        {problem && (
          <p className="studio-dialog-failure" role="alert">
            {problem}
          </p>
        )}
        <footer className="studio-dialog-footer">
          <div className="studio-dialog-footer-end">
            <button type="button" className="studio-button" onClick={close}>
              {t("proofSheet.cancel")}
            </button>
            <button
              type="button"
              className="studio-button is-primary"
              disabled={busy || !preview || (!draft.pdf && !draft.jpeg) || (draft.watermark && !watermark)}
              onClick={() => void create()}
            >
              {t("proofSheet.create")}
            </button>
          </div>
        </footer>
      </div>
    </div>,
    document.body,
  );
}
