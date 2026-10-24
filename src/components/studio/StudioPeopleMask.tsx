import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { getTransformAdjustments } from "../../hooks/useAiMasking";
import { Invokes } from "../ui/AppProperties";
import type { SubMask } from "../panel/right/Masks";
import { PEOPLE_PARTS, type PeoplePart } from "../../utils/maskKinds";
import { aiErrorMessage } from "../../utils/aiErrors";
import AdjustmentControl from "./AdjustmentControl";

/** A person found in the photo (`detect_people`). */
export interface PersonSummary {
  index: number;
  /** x, y, width, height of the face as fractions of the photo. */
  face: [number, number, number, number];
  /** JPEG data URL of the face. */
  thumbnail: string;
  /** Eyes, brows, lips and teeth can be selected (not in profile views). */
  hasFeatures: boolean;
}

/** Parts drawn from the face's landmarks, which profile views lack. */
const FEATURE_PARTS: PeoplePart[] = ["eyebrows", "sclera", "iris", "lips", "teeth"];

interface Props {
  sub: SubMask;
  setParameters(patch: Record<string, unknown>): void;
  onInteraction(dragging: boolean): void;
}

/**
 * Lightroom's People mask: everyone or one person (picked by their face),
 * and which of their parts. Changing either computes the mask again.
 */
export default function StudioPeopleMask({ sub, setParameters, onInteraction }: Props) {
  const { t } = useTranslation();
  const path = useEditorStore((s) => s.selectedImage?.path);
  const adjustments = useEditorStore((s) => s.adjustments);
  const geometry = useMemo(() => getTransformAdjustments(adjustments), [adjustments]);
  const geometryKey = JSON.stringify(geometry);
  const [people, setPeople] = useState<PersonSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!path) return;
    let current = true;
    setPeople(null);
    setError(null);
    invoke<PersonSummary[]>(Invokes.DetectPeople, { jsAdjustments: JSON.parse(geometryKey), path })
      .then((found) => current && setPeople(found))
      .catch((e) => current && setError(aiErrorMessage(e)));
    return () => {
      current = false;
    };
  }, [path, geometryKey]);

  const parameters = sub.parameters as { parts?: PeoplePart[]; person?: number | null };
  const parts = parameters.parts ?? [];
  const person = parameters.person ?? null;
  const chosen = person === null ? null : people?.find((p) => p.index === person);
  const featuresMissing = Boolean(chosen && !chosen.hasFeatures);
  const choose = (patch: Record<string, unknown>) => setParameters({ ...patch, maskDataBase64: null });
  // The entire person holds every part: a part ticked beside it would change
  // nothing on the photo, so a part replaces it and it replaces the parts.
  const togglePart = (part: PeoplePart) =>
    choose({
      parts: parts.includes(part)
        ? parts.filter((p) => p !== part)
        : part === "entirePerson"
          ? [part]
          : [...parts.filter((p) => p !== "entirePerson"), part],
    });

  return (
    <div className="studio-people" role="group" aria-label={t("studio.people.aria")}>
      <div className="studio-people-list" role="radiogroup" aria-label={t("studio.people.personAria")}>
        <button
          role="radio"
          aria-checked={person === null}
          className="studio-people-everyone"
          onClick={() => choose({ person: null })}
        >
          {t("studio.people.everyone")}
        </button>
        {people?.map((p) => (
          <button
            key={p.index}
            role="radio"
            aria-checked={person === p.index}
            title={
              p.hasFeatures
                ? t("studio.people.person", { n: p.index + 1 })
                : t("studio.people.personProfile", { n: p.index + 1 })
            }
            onClick={() => choose({ person: p.index })}
          >
            <img src={p.thumbnail} alt={t("studio.people.person", { n: p.index + 1 })} />
          </button>
        ))}
      </div>
      {people === null && !error && <p className="studio-hint">{t("studio.people.finding")}</p>}
      {people?.length === 0 && (
        <p className="studio-hint" role="note">
          {t("studio.people.noFaces")}
        </p>
      )}
      {error && (
        <p className="studio-hint" role="alert">
          {error}
        </p>
      )}
      <div className="studio-people-parts" role="group" aria-label={t("studio.people.partsAria")}>
        {PEOPLE_PARTS.map(({ part }) => (
          <label key={part} className="studio-check">
            <input
              type="checkbox"
              checked={parts.includes(part)}
              disabled={featuresMissing && FEATURE_PARTS.includes(part)}
              onChange={() => togglePart(part)}
            />
            {t(`studio.people.parts.${part}`)}
          </label>
        ))}
      </div>
      <AdjustmentControl
        label={t("editor.masks.params.grow")}
        min={-100}
        max={100}
        value={Number(sub.parameters.grow ?? 0)}
        defaultValue={0}
        onInteraction={onInteraction}
        onChange={(grow) => setParameters({ grow })}
      />
      <AdjustmentControl
        label={t("editor.masks.params.feather")}

        min={0}
        max={100}
        value={Number(sub.parameters.feather ?? 0)}
        defaultValue={0}
        onInteraction={onInteraction}
        onChange={(feather) => setParameters({ feather })}
      />
    </div>
  );
}
