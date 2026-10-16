import {
  DndContext,
  DragOverlay,
  PointerSensor,
  pointerWithin,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import {
  NamingDragButton,
  NamingDropSlot,
  type NamingDragData,
} from "./NamingDrag";
import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowLeft, ArrowRight, X } from "lucide-react";
import {
  namingExample,
  dropNamingBlock,
  MAX_SEQUENCE_START,
  validSequenceStart,
  validNamingTemplate,
  namingPresets,
  namingTokens,
  splitTemplate,
  type NamePreview,
  type NamingMode,
} from "../../utils/namingTemplates";
import "./naming-editor.css";

type Saved = { name: string; template: string; sequenceStart?: number };
function readPresets(key: string): Saved[] {
  try {
    const items: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
    return Array.isArray(items)
      ? items
          .filter(
            (p): p is Saved =>
              p && typeof p.name === "string" && typeof p.template === "string",
          )
          .slice(0, 50)
      : [];
  } catch {
    return [];
  }
}

export default function NamingEditor({
  value,
  onChange,
  mode,
  label,
  original = "DSC01234.RAF",
  extension = "jpg",
  job,
  total = 1,
  sequenceStart = 1,
  onSequenceStartChange,
  preview,
}: {
  value: string;
  onChange: (value: string) => void;
  mode: NamingMode;
  label: string;
  original?: string;
  extension?: string;
  job?: string;
  total?: number;
  sequenceStart?: number;
  onSequenceStartChange?: (value: number) => void;
  /** The backend's name for the first source photograph (its own EXIF and
   * sidecar); without it the example uses illustrative values. */
  preview?: NamePreview | null;
}) {
  const { t } = useTranslation();
  const id = useId();
  const input = useRef<HTMLInputElement>(null);
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
  );
  const [dragging, setDragging] = useState<NamingDragData | null>(null);
  const dragTemplate = useRef(value);
  const tokenLabel = (text: string) => {
    const token = namingTokens(mode).find((item) => item.value === text);
    return token ? t(`naming.tokens.${token.label}`) : text;
  };
  const key = `rawtonic.naming.${mode}`;
  const [saved, setSaved] = useState(() => readPresets(key));
  const [name, setName] = useState("");
  const [storageError, setStorageError] = useState(false);
  const [active, setActive] = useState<number | null>(null);
  const valid = validNamingTemplate(value, mode);
  useEffect(() => {
    input.current?.setCustomValidity(valid ? "" : t("naming.invalid"));
  }, [valid, t]);
  const parts = splitTemplate(value);
  const tokens = namingTokens(mode);
  const presets = namingPresets(mode);
  const selected = presets.findIndex((p) => p.template === value);
  const custom = saved.findIndex(
    (p) =>
      p.template === value &&
      (!onSequenceStartChange || (p.sequenceStart ?? 1) === sequenceStart),
  );
  const persist = (next: Saved[]) => {
    try {
      localStorage.setItem(key, JSON.stringify(next));
      setSaved(next);
      setStorageError(false);
    } catch {
      setStorageError(true);
    }
  };
  const insert = (text: string) => {
    const start = input.current?.selectionStart ?? value.length;
    const end = input.current?.selectionEnd ?? start;
    onChange(value.slice(0, start) + text + value.slice(end));
    setActive(null);
    requestAnimationFrame(() => {
      input.current?.focus();
      input.current?.setSelectionRange(
        start + text.length,
        start + text.length,
      );
    });
  };
  const move = (direction: number) => {
    if (
      active === null ||
      active + direction < 0 ||
      active + direction >= parts.length
    )
      return;
    [parts[active], parts[active + direction]] = [
      parts[active + direction],
      parts[active],
    ];
    onChange(parts.join(""));
    setActive(active + direction);
  };
  return (
    <div className="naming-editor">
      <label htmlFor={id}>{label}</label>
      <div className="naming-preset-row">
        <select
          aria-label={t("naming.preset")}
          value={
            selected >= 0
              ? `builtin:${selected}`
              : custom >= 0
                ? `saved:${custom}`
                : ""
          }
          onChange={(e) => {
            const [kind, index] = e.target.value.split(":");
            const preset = (kind === "builtin" ? presets : saved)[
              Number(index)
            ];
            if (preset) {
              onChange(preset.template);
              if (kind === "saved" && onSequenceStartChange) {
                const start = (preset as Saved).sequenceStart ?? 1;
                onSequenceStartChange(validSequenceStart(start) ? start : 1);
              }
              setActive(null);
            }
          }}
        >
          <option value="">{t("naming.custom")}</option>
          {presets.map((p, i) => (
            <option key={p.name} value={`builtin:${i}`}>
              {t(`naming.presets.${p.name}`)}
            </option>
          ))}
          {saved.map((p, i) => (
            <option key={p.name} value={`saved:${i}`}>
              {p.name}
            </option>
          ))}
        </select>
      </div>
      <input
        id={id}
        ref={input}
        value={value}
        required
        pattern="[^/\\\\]+"
        spellCheck={false}
        onChange={(e) => {
          onChange(e.target.value);
          setActive(null);
        }}
        aria-invalid={!valid}
        aria-describedby={`${id}-example${!valid ? ` ${id}-error` : ""}`}
      />
      {!valid && (
        <p className="naming-error" id={`${id}-error`} role="alert">
          {t("naming.invalid")}
        </p>
      )}
      {onSequenceStartChange && (
        <label className="naming-counter">
          <span>{t("naming.counterStart")}</span>
          <input
            type="number"
            min={0}
            max={MAX_SEQUENCE_START}
            step={1}
            required
            value={Number.isFinite(sequenceStart) ? sequenceStart : ""}
            aria-invalid={!validSequenceStart(sequenceStart)}
            onChange={(e) => onSequenceStartChange(e.target.valueAsNumber)}
          />
        </label>
      )}
      <div className="naming-example" id={`${id}-example`} aria-live="polite">
        <span>{t("naming.example")}</span>
        <code>
          {preview && valid
            ? preview.name
            : namingExample(
                value,
                mode,
                original,
                extension,
                job,
                validSequenceStart(sequenceStart) ? sequenceStart : 1,
                total,
              )}
        </code>
        {preview && valid && (
          <small className="naming-date-source">
            {t(`naming.dateSources.${preview.dateSource}`)}
            {preview.sidecar ? ` · ${t("naming.sidecarCopied")}` : ""}
          </small>
        )}
      </div>
      <details className="naming-builder">
        <summary>{t("naming.edit")}</summary>
        <p>{t("naming.hint")}</p>
        <DndContext
          sensors={sensors}
          accessibility={{
            screenReaderInstructions: { draggable: t("naming.keyboardHint") },
          }}
          collisionDetection={pointerWithin}
          onDragStart={({ active }) => {
            dragTemplate.current = value;
            setDragging(active.data.current as NamingDragData);
          }}
          onDragCancel={() => setDragging(null)}
          onDragEnd={({ active, over }) => {
            if (over && dragTemplate.current === value) {
              const data = active.data.current as NamingDragData;
              onChange(
                dropNamingBlock(
                  parts,
                  over.data.current?.index as number,
                  data.text,
                  data.kind === "part" ? data.index : undefined,
                ),
              );
              setActive(null);
            }
            setDragging(null);
          }}
        >
          <div className="naming-parts" aria-label={t("naming.parts")}>
            {parts.map((part, index) => (
              <NamingDropSlot
                key={index}
                index={index}
                label={t("naming.dropHere")}
                dragging={!!dragging}
              >
                <NamingDragButton
                  id={`part-${index}`}
                  data={{ kind: "part", index, text: part }}
                  pressed={active === index}
                  className={part.startsWith("{") ? "is-token" : ""}
                  onClick={() => setActive(index)}
                >
                  {tokenLabel(part)}
                </NamingDragButton>
              </NamingDropSlot>
            ))}
            <NamingDropSlot
              index={parts.length}
              label={t("naming.dropHere")}
              dragging={!!dragging}
            />
          </div>
          {active !== null && active < parts.length && (
            <div className="naming-part-actions">
              <button
                type="button"
                aria-label={t("naming.moveLeft")}
                disabled={active === 0}
                onClick={() => move(-1)}
              >
                <ArrowLeft size={14} />
              </button>
              <button
                type="button"
                aria-label={t("naming.moveRight")}
                disabled={active === parts.length - 1}
                onClick={() => move(1)}
              >
                <ArrowRight size={14} />
              </button>
              <button
                type="button"
                aria-label={t("naming.remove")}
                onClick={() => {
                  onChange(parts.filter((_, i) => i !== active).join(""));
                  setActive(null);
                }}
              >
                <X size={14} />
              </button>
            </div>
          )}
          <div className="naming-tokens" aria-label={t("naming.addToken")}>
            {tokens.map((token) => (
              <NamingDragButton
                key={token.value}
                id={`token-${token.value}`}
                data={{ kind: "token", text: token.value }}
                title={token.value}
                onClick={() => insert(token.value)}
              >
                {t(`naming.tokens.${token.label}`)}
              </NamingDragButton>
            ))}
            {["_", "-", " "].map((separator) => (
              <NamingDragButton
                key={separator}
                id={`separator-${separator}`}
                data={{ kind: "token", text: separator }}
                onClick={() => insert(separator)}
              >
                {separator === " " ? t("naming.space") : separator}
              </NamingDragButton>
            ))}
          </div>
          <DragOverlay dropAnimation={null}>
            {dragging && (
              <span className="naming-drag-preview">
                {tokenLabel(dragging.text)}
              </span>
            )}
          </DragOverlay>
        </DndContext>
        <small>{t("naming.dateNote")}</small>
        <div className="naming-save">
          <input
            aria-label={t("naming.presetName")}
            placeholder={t("naming.presetName")}
            value={name}
            maxLength={80}
            onChange={(e) => setName(e.target.value)}
          />
          <button
            type="button"
            disabled={
              !name.trim() ||
              !valid ||
              !validSequenceStart(sequenceStart) ||
              saved.length >= 50
            }
            onClick={() => {
              persist([
                ...saved.filter((p) => p.name !== name.trim()),
                {
                  name: name.trim(),
                  template: value,
                  ...(onSequenceStartChange ? { sequenceStart } : {}),
                },
              ]);
              setName("");
            }}
          >
            {t("naming.save")}
          </button>
          {custom >= 0 && (
            <button
              type="button"
              onClick={() =>
                persist(saved.filter((_, index) => index !== custom))
              }
            >
              {t("naming.deletePreset")}
            </button>
          )}
        </div>
        {storageError && <p role="alert">{t("naming.saveError")}</p>}
      </details>
    </div>
  );
}
