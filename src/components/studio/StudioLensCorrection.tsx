import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { isCurrentEditorImage, useEditorActions } from "../../hooks/useEditorActions";
import { CAMERA_CA_VERSION, INITIAL_ADJUSTMENTS, autoCaOn, type Adjustments } from "../../utils/adjustments";
import { Invokes } from "../ui/AppProperties";
import AdjustmentControl from "./AdjustmentControl";
import { ToolGroup } from "./StudioDevelop";

const exifNumber = (value: string | null | undefined) => {
  const parsed = parseFloat(value ?? "");
  return Number.isFinite(parsed) ? parsed : 0;
};

/** Every lens-profile setting back to "no profile". */
export const LENS_PROFILE_OFF: Partial<Adjustments> = {
  lensMaker: INITIAL_ADJUSTMENTS.lensMaker,
  lensModel: INITIAL_ADJUSTMENTS.lensModel,
  lensDistortionParams: INITIAL_ADJUSTMENTS.lensDistortionParams,
  lensCorrectionMode: INITIAL_ADJUSTMENTS.lensCorrectionMode,
  lensDistortionAmount: INITIAL_ADJUSTMENTS.lensDistortionAmount,
  lensVignetteAmount: INITIAL_ADJUSTMENTS.lensVignetteAmount,
  lensTcaAmount: INITIAL_ADJUSTMENTS.lensTcaAmount,
  lensDistortionEnabled: INITIAL_ADJUSTMENTS.lensDistortionEnabled,
  lensTcaEnabled: INITIAL_ADJUSTMENTS.lensTcaEnabled,
  lensVignetteEnabled: INITIAL_ADJUSTMENTS.lensVignetteEnabled,
};

type Status = "idle" | "detecting" | "not_found";

/** The camera's own correction data for a RAW (`get_camera_lens_profile`). */
interface CameraLensProfile {
  maker: string;
  params: Adjustments["lensDistortionParams"];
  vignetting: boolean;
}

/** Lightroom's Lens Corrections (Profile) / Capture One's Lens Correction:
 * profile corrections from the lens database, detected from the photo's EXIF
 * or chosen by maker and model, with amounts for distortion, vignetting and
 * chromatic aberration (0–200%). */
export default function StudioLensCorrection() {
  const { t } = useTranslation();
  const selectedImage = useEditorStore((s) => s.selectedImage);
  const adjustments = useEditorStore((s) => s.adjustments);
  const { setAdjustments } = useEditorActions();
  const setEditor = useEditorStore((s) => s.setEditor);
  const [makers, setMakers] = useState<string[]>([]);
  const [lenses, setLenses] = useState<string[]>([]);
  const [status, setStatus] = useState<Status>("idle");
  const [camera, setCamera] = useState<CameraLensProfile | null>(null);
  const cameraMode = adjustments.lensCorrectionMode === "camera";
  const enabled = cameraMode
    ? Boolean(adjustments.lensDistortionParams)
    : Boolean(adjustments.lensMaker && adjustments.lensModel);
  // Keeps its look while the next photograph loads (the tools are inert).
  const ready = useEditorStore(panelsShowReady);
  const exif = selectedImage?.exif ?? {};

  useEffect(() => {
    invoke<string[]>(Invokes.GetLensfunMakers)
      .then((list) => setMakers(Array.isArray(list) ? list : []))
      .catch(() => setMakers([]));
  }, []);
  useEffect(() => {
    if (!adjustments.lensMaker) return setLenses([]);
    let current = true;
    invoke<string[]>(Invokes.GetLensfunLensesForMaker, {
      maker: adjustments.lensMaker,
    })
      .then((list) => current && setLenses(Array.isArray(list) ? list : []))
      .catch(() => current && setLenses([]));
    return () => {
      current = false;
    };
  }, [adjustments.lensMaker]);
  useEffect(() => setStatus("idle"), [selectedImage?.path]);
  useEffect(() => {
    setCamera(null);
    const path = selectedImage?.path;
    if (!path) return;
    let current = true;
    invoke<CameraLensProfile | null>(Invokes.GetCameraLensProfile, { path })
      .then((profile) => current && setCamera(profile ?? null))
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [selectedImage?.path]);
  const applyCamera = (profile: CameraLensProfile) =>
    setAdjustments((prev) => ({
      ...prev,
      lensMaker: null,
      lensModel: null,
      lensDistortionParams: profile.params,
      lensCorrectionMode: "camera",
      cameraCaVersion: CAMERA_CA_VERSION,
    }));

  // With the photo's path the parameters are looked up from its EXIF as a
  // save would (crop factor included): the save keeps what arrives.
  const params = (maker: string, model: string) =>
    invoke<Adjustments["lensDistortionParams"]>(
      Invokes.GetLensDistortionParams,
      {
        maker,
        model,
        focalLength: exifNumber(exif.FocalLength),
        aperture: exifNumber(exif.FNumber),
        distance: exifNumber(exif.SubjectDistance),
        path: selectedImage?.path ?? null,
      },
    ).catch(() => null);

  const enable = async () => {
    // The camera's own data first (Capture One, in-camera JPEGs), then the
    // lens database.
    if (camera) return applyCamera(camera);
    if (!exif.Make || !(exif.LensModel || exif.Model))
      return setStatus("not_found");
    // The detected lens belongs to the photograph it was detected for.
    const path = selectedImage?.path;
    if (!path) return;
    const stillOpen = () => {
      if (isCurrentEditorImage(path)) return true;
      setStatus("idle");
      return false;
    };
    setStatus("detecting");
    try {
      const found = await invoke<[string, string] | null>(
        Invokes.AutodetectLens,
        {
          maker: exif.Make,
          model: exif.LensModel ?? "",
          cameraModel: exif.Model ?? "",
        },
      );
      if (!stillOpen()) return;
      if (!found) return setStatus("not_found");
      const [maker, model] = found;
      const distortion = await params(maker, model);
      if (!stillOpen()) return;
      setAdjustments((prev) => ({
        ...prev,
        lensMaker: maker,
        lensModel: model,
        lensDistortionParams: distortion,
        lensCorrectionMode: "auto",
      }));
      setStatus("idle");
    } catch {
      if (stillOpen()) setStatus("not_found");
    }
  };
  const chooseMaker = (maker: string) =>
    setAdjustments((prev) => ({
      ...prev,
      lensMaker: maker || null,
      lensModel: null,
      lensDistortionParams: null,
      lensCorrectionMode: "manual",
    }));
  const chooseModel = async (model: string) => {
    const maker = adjustments.lensMaker;
    const path = selectedImage?.path;
    if (!maker || !path) return;
    const distortion = model ? await params(maker, model) : null;
    if (!isCurrentEditorImage(path)) return;
    setAdjustments((prev) => ({
      ...prev,
      lensModel: model || null,
      lensDistortionParams: distortion,
      lensCorrectionMode: "manual",
    }));
  };
  const amount = (
    label: string,
    key: "lensDistortionAmount" | "lensVignetteAmount" | "lensTcaAmount",
    // The camera's data has no curve for this correction.
    unavailable = false,
  ) => (
    <AdjustmentControl
      label={label}
      value={adjustments[key] ?? 100}
      min={0}
      max={200}
      step={1}
      defaultValue={100}
      unit="%"
      disabled={!ready || !enabled || unavailable}
      onChange={(value) =>
        setAdjustments((prev) => ({ ...prev, [key]: value }))
      }
      onInteraction={(active) => setEditor({ isSliderDragging: active })}
    />
  );

  return (
    <ToolGroup
      title="Lens correction"
      label={t("studio.lens.title")}
      onReset={() =>
        setAdjustments((prev) => ({
          ...prev,
          ...LENS_PROFILE_OFF,
          diffractionCorrection: undefined,
        }))
      }
    >
      <label className="studio-check studio-lens-enable">
        <input
          type="checkbox"
          checked={enabled}
          disabled={!ready || status === "detecting"}
          onChange={(e) =>
            e.target.checked
              ? void enable()
              : setAdjustments((prev) => ({ ...prev, ...LENS_PROFILE_OFF }))
          }
        />
        {t("studio.lens.enable")}
      </label>
      {status === "detecting" && (
        <p className="studio-metadata-note">{t("studio.lens.lookingUp")}</p>
      )}
      {status === "not_found" && (
        <p className="studio-metadata-note">
          {exif.LensModel || exif.Model
            ? t("studio.lens.notFound", { lens: exif.LensModel || exif.Model })
            : t("studio.lens.notFoundPhoto")}
        </p>
      )}
      {cameraMode && enabled ? (
        <div className="studio-metadata-field">
          <span>{t("studio.lens.profile")}</span>
          <span className="studio-lens-profile">
            {camera
              ? t("studio.lens.cameraDataOf", { maker: camera.maker })
              : t("studio.lens.cameraData")}
            <button
              className="studio-text-button"
              title={t("studio.lens.useDatabaseTitle")}
              onClick={() => chooseMaker("")}
            >
              {t("studio.lens.useDatabase")}
            </button>
          </span>
        </div>
      ) : (
        <>
          <label className="studio-metadata-field">
            <span>{t("studio.lens.make")}</span>
            <select
              aria-label={t("studio.lens.aria.make")}
              value={adjustments.lensMaker ?? ""}
              disabled={!ready}
              onChange={(e) => chooseMaker(e.target.value)}
            >
              <option value="">{t("studio.lens.none")}</option>
              {makers.map((maker) => (
                <option key={maker} value={maker}>
                  {maker}
                </option>
              ))}
            </select>
          </label>
          <label className="studio-metadata-field">
            <span>{t("studio.lens.model")}</span>
            <select
              aria-label={t("studio.lens.aria.model")}
              value={adjustments.lensModel ?? ""}
              disabled={!ready || !adjustments.lensMaker}
              onChange={(e) => void chooseModel(e.target.value)}
            >
              <option value="">
                {adjustments.lensMaker ? t("studio.lens.chooseLens") : "—"}
              </option>
              {lenses.map((lens) => (
                <option key={lens} value={lens}>
                  {lens}
                </option>
              ))}
            </select>
          </label>
          {camera && (
            <button
              className="studio-text-button"
              onClick={() => applyCamera(camera)}
            >
              {t("studio.lens.useCameraData", { maker: camera.maker })}
            </button>
          )}
        </>
      )}
      {enabled && !adjustments.lensDistortionParams && (
        <p className="studio-metadata-note">{t("studio.lens.noFocalData")}</p>
      )}
      {amount(t("studio.lens.distortion"), "lensDistortionAmount")}
      {amount(
        t("studio.lens.vignetting"),
        "lensVignetteAmount",
        cameraMode && !camera?.vignetting,
      )}
      {amount(t("studio.lens.chromaticShort"), "lensTcaAmount", cameraMode)}
      <label
        className="studio-check studio-lens-enable"
        title={t("studio.lens.diffractionTitle")}
      >
        <input
          type="checkbox"
          checked={!!adjustments.diffractionCorrection}
          disabled={!ready}
          onChange={(e) =>
            setAdjustments((prev) => ({
              ...prev,
              diffractionCorrection: e.target.checked || undefined,
            }))
          }
        />
        {t("studio.lens.diffraction")}
      </label>
      <label
        className="studio-check studio-lens-enable"
        title={t("studio.lens.removeCaTitle")}
      >
        <input
          type="checkbox"
          aria-label={t("studio.lens.removeCa")}
          checked={autoCaOn(adjustments)}
          disabled={!ready}
          onChange={(e) => {
            const on = e.target.checked;
            setAdjustments((prev) => {
              // The switch is stored only where it differs from the recipe's
              // default (on for new RAW edits without lens CA data).
              const byDefault = autoCaOn({ ...prev, autoChromaticAberration: undefined });
              return { ...prev, autoChromaticAberration: on === byDefault ? undefined : on };
            });
          }}
        />
        {t("studio.lens.removeCa")}
      </label>
    </ToolGroup>
  );
}
