import {
  DndContext,
  DragOverlay,
  PointerSensor,
  pointerWithin,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import { NamingDragButton, NamingDropSlot, type NamingDragData } from "./NamingDrag";
import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import {
  COUNTER_DIGITS,
  DATE_FORMATS,
  NAMING_GROUPS,
  TIME_FORMATS,
  counterDigits,
  formatToken,
  insertToken,
  keepCounterSeparator,
  MAX_SEQUENCE_START,
  moveToken,
  namingExample,
  namingPresets,
  namingTokens,
  parseToken,
  readNamingPresets,
  removeToken,
  swapTokens,
  templateSegments,
  validNamingTemplate,
  validSequenceStart,
  withCounterDigits,
  writeNamingPresets,
  joinSegments,
  type NamingKind,
  type NamingMode,
  type NamingPreview,
  type SavedNaming,
} from "../../utils/namingTemplates";
import "./naming-editor.css";

/**
 * The one naming editor of import, rename and export (Capture One's format
 * field): tokens are chips in an editable format with free text between
 * them, added by double-click (or Enter) or drag from grouped lists; the
 * counter's digits and the date's format are choices on their chips. The
 * example is the backend's name for the selected photo when `preview` is
 * given. `kind="folder"` edits a folder template (`/` separates folders,
 * no presets).
 */
export default function NamingEditor({
  value,
  onChange,
  mode,
  label,
  kind = "file",
  original = "DSC01234.RAF",
  extension = "jpg",
  job,
  total = 1,
  sequenceStart = 1,
  onSequenceStartChange,
  preview,
  invalidMessage,
}: {
  value: string;
  onChange: (value: string) => void;
  mode: NamingMode;
  label: string;
  kind?: NamingKind;
  original?: string;
  extension?: string;
  job?: string;
  total?: number;
  sequenceStart?: number;
  onSequenceStartChange?: (value: number) => void;
  preview?: NamingPreview | null;
  /** Why the caller refuses the template (shown instead of the editor's
   * own message, and marking it invalid). */
  invalidMessage?: string | null;
}) {
  const { t } = useTranslation();
  const id = useId();
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 6 } }));
  const [dragging, setDragging] = useState<NamingDragData | null>(null);
  const dragTemplate = useRef(value);
  const texts = useRef<(HTMLInputElement | null)[]>([]);
  const chips = useRef<(HTMLButtonElement | null)[]>([]);
  // Where a token goes: free text `slot` at `offset` (the last caret).
  const caret = useRef<{ slot: number; offset: number } | null>(null);
  const [saved, setSaved] = useState<SavedNaming[]>(() => readNamingPresets());
  const [name, setName] = useState("");
  const [storageError, setStorageError] = useState(false);
  const folder = kind === "folder";
  const valid = validNamingTemplate(value, mode, kind) && !invalidMessage;
  const code = useRef<HTMLInputElement>(null);
  useEffect(() => {
    code.current?.setCustomValidity(valid ? "" : invalidMessage || t("naming.invalid"));
  }, [valid, invalidMessage, t]);
  const { text, tokens } = templateSegments(value);
  const vocabulary = namingTokens(mode);
  const tokenName = (part: string) => {
    const { name } = parseToken(part);
    const token = vocabulary.find((item) => item.name === name);
    if (!token) return part;
    return token.name === "job" && mode === "export" ? t("naming.tokens.jobName") : t(`naming.tokens.${token.label}`);
  };
  const presets = folder ? [] : namingPresets(mode);
  const usable = saved.map((p, index) => ({ ...p, index })).filter((p) => validNamingTemplate(p.template, mode));
  const selected = presets.findIndex((p) => p.template === value);
  const custom = usable.find(
    (p) => p.template === value && (!onSequenceStartChange || (p.sequenceStart ?? 1) === sequenceStart),
  );
  const persist = (next: SavedNaming[]) => {
    try {
      writeNamingPresets(next);
      setSaved(next);
      setStorageError(false);
    } catch {
      setStorageError(true);
    }
  };
  const focusText = (slot: number, offset: number | "end") =>
    requestAnimationFrame(() => {
      const input = texts.current[slot];
      if (!input) return;
      input.focus();
      const at = offset === "end" ? input.value.length : offset;
      input.setSelectionRange(at, at);
      caret.current = { slot, offset: at };
    });
  const add = (token: string) => {
    const at = caret.current ?? { slot: text.length - 1, offset: text[text.length - 1].length };
    // Free text (separators) joins the text; a token becomes a chip.
    if (!token.startsWith("{")) {
      const next = [...text];
      next[at.slot] = next[at.slot].slice(0, at.offset) + token + next[at.slot].slice(at.offset);
      onChange(joinSegments(next, tokens));
      focusText(at.slot, at.offset + token.length);
      return;
    }
    onChange(insertToken(value, at.slot, at.offset, token));
    focusText(at.slot + 1, 0);
  };
  const remember = (slot: number, input: HTMLInputElement) => {
    caret.current = { slot, offset: input.selectionStart ?? input.value.length };
  };
  const removeChip = (index: number) => {
    const offset = text[index].length;
    onChange(removeToken(value, index));
    focusText(index, offset);
  };
  const setTokenOption = (index: number, option: string | undefined) => {
    const { name, transforms } = parseToken(tokens[index]);
    const next = [...tokens];
    next[index] = formatToken(name, option, transforms);
    onChange(joinSegments(text, next));
  };
  const textKeys = (slot: number) => (event: KeyboardEvent<HTMLInputElement>) => {
    const input = event.currentTarget;
    const collapsed = input.selectionStart === input.selectionEnd;
    const start = input.selectionStart ?? 0;
    if (event.key === "Backspace" && collapsed && start === 0 && slot > 0) {
      event.preventDefault();
      removeChip(slot - 1);
    } else if (event.key === "Delete" && collapsed && start === input.value.length && slot < tokens.length) {
      event.preventDefault();
      onChange(removeToken(value, slot));
      focusText(slot, start);
    } else if (event.key === "ArrowLeft" && collapsed && start === 0 && slot > 0) {
      event.preventDefault();
      chips.current[slot - 1]?.focus();
    } else if (event.key === "ArrowRight" && collapsed && start === input.value.length && slot < tokens.length) {
      event.preventDefault();
      chips.current[slot]?.focus();
    }
  };
  const chipKeys = (index: number) => (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key === "Backspace" || event.key === "Delete") {
      event.preventDefault();
      removeChip(index);
    } else if (event.altKey && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
      event.preventDefault();
      const direction = event.key === "ArrowLeft" ? -1 : 1;
      onChange(swapTokens(value, index, direction));
      requestAnimationFrame(() => chips.current[index + direction]?.focus());
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      focusText(index, "end");
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      focusText(index + 1, 0);
    }
  };
  const listKeys = (token: string) => (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key === "Enter" || event.key === " ") {
      // Not the dialog's Enter (Rename saves on it).
      event.preventDefault();
      event.stopPropagation();
      add(token);
    }
  };
  const digits = counterDigits(value);
  const separators = folder ? ["/", "_", "-", " "] : ["_", "-", " ", "."];
  const example = preview && valid
    ? [preview.folder, preview.name].filter(Boolean).join("/")
    : namingExample(
        value,
        mode,
        original,
        extension,
        job,
        validSequenceStart(sequenceStart) ? sequenceStart : 1,
        total,
        kind,
      );

  return (
    <div className="naming-editor">
      <span className="naming-label" id={`${id}-label`}>{label}</span>
      {!folder && (
        <div className="naming-preset-row">
          <select
            aria-label={t("naming.preset")}
            value={selected >= 0 ? `builtin:${selected}` : custom ? `saved:${custom.index}` : ""}
            onChange={(e) => {
              const [where, index] = e.target.value.split(":");
              const preset = where === "builtin" ? presets[Number(index)] : saved[Number(index)];
              if (!preset) return;
              onChange(preset.template);
              if (where === "saved" && onSequenceStartChange) {
                const start = (preset as SavedNaming).sequenceStart ?? 1;
                onSequenceStartChange(validSequenceStart(start) ? start : 1);
              }
              caret.current = null;
            }}
          >
            <option value="">{t("naming.custom")}</option>
            {presets.map((p, i) => (
              <option key={p.name} value={`builtin:${i}`}>
                {t(`naming.presets.${p.name}`)}
              </option>
            ))}
            {usable.map((p) => (
              <option key={p.name} value={`saved:${p.index}`}>
                {p.name}
              </option>
            ))}
          </select>
        </div>
      )}
      <DndContext
        sensors={sensors}
        accessibility={{ screenReaderInstructions: { draggable: t("naming.keyboardHint") } }}
        collisionDetection={pointerWithin}
        onDragStart={({ active }) => {
          dragTemplate.current = value;
          setDragging(active.data.current as NamingDragData);
        }}
        onDragCancel={() => setDragging(null)}
        onDragEnd={({ active, over }) => {
          if (over && dragTemplate.current === value) {
            const data = active.data.current as NamingDragData;
            const slot = over.data.current?.index as number;
            onChange(
              data.kind === "chip"
                ? moveToken(value, data.index, slot)
                : data.text.startsWith("{")
                  ? insertToken(value, slot, text[slot]?.length ?? 0, data.text)
                  : joinSegments(
                      text.map((part, i) => (i === slot ? part + data.text : part)),
                      tokens,
                    ),
            );
            caret.current = null;
          }
          setDragging(null);
        }}
      >
        <div
          className={`naming-field${valid ? "" : " is-invalid"}`}
          role="group"
          aria-label={t("naming.parts")}
          aria-describedby={`${id}-field-hint ${id}-example${valid ? "" : ` ${id}-error`}`}
        >
          {text.map((part, slot) => (
            <NamingDropSlot key={`slot-${slot}`} index={slot}>
              <input
                ref={(node) => {
                  texts.current[slot] = node;
                }}
                className="naming-text"
                aria-label={t("naming.textAt", { n: slot + 1 })}
                value={part}
                size={Math.max(1, part.length)}
                spellCheck={false}
                autoCapitalize="off"
                autoCorrect="off"
                onChange={(e) => {
                  const next = [...text];
                  const typed = e.target.value;
                  // Replacing the separator before the counter with plain text
                  // ("Wedding" over "_") keeps it: Wedding_0001, not Wedding0001.
                  // Deleting the kept separator afterwards is the user's choice.
                  const kept = keepCounterSeparator(part, typed, tokens[slot]);
                  next[slot] = kept;
                  onChange(joinSegments(next, tokens));
                  remember(slot, e.target);
                  if (kept !== typed) focusText(slot, typed.length);
                }}
                onSelect={(e) => remember(slot, e.currentTarget)}
                onFocus={(e) => remember(slot, e.currentTarget)}
                onKeyDown={textKeys(slot)}
              />
              {slot < tokens.length && (
                <span className={`naming-chip${validNamingTemplate(tokens[slot], mode, kind) ? "" : " is-invalid"}`}>
                  <NamingDragButton
                    id={`chip-${slot}`}
                    data={{ kind: "chip", index: slot, text: tokens[slot] }}
                    className="naming-chip-label"
                    title={tokens[slot]}
                    buttonRef={(node) => {
                      chips.current[slot] = node;
                    }}
                    onKeyDown={chipKeys(slot)}
                  >
                    {tokenName(tokens[slot])}
                  </NamingDragButton>
                  <ChipOption part={tokens[slot]} onOption={(option) => setTokenOption(slot, option)} />
                  <button
                    type="button"
                    className="naming-chip-remove"
                    aria-label={t("naming.removeToken", { name: tokenName(tokens[slot]) })}
                    onClick={() => removeChip(slot)}
                  >
                    <X size={11} />
                  </button>
                </span>
              )}
            </NamingDropSlot>
          ))}
        </div>
        <small className="naming-hint" id={`${id}-field-hint`}>
          {t("naming.fieldHint")}
        </small>
        {!valid && (
          <p className="naming-error" id={`${id}-error`} role="alert">
            {invalidMessage || t("naming.invalid")}
          </p>
        )}
        {!folder && (
          <div className="naming-counter-row">
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
            <label className="naming-counter">
              <span>{t("naming.digits")}</span>
              <select
                value={digits ?? ""}
                disabled={digits === null}
                onChange={(e) => onChange(withCounterDigits(value, e.target.value))}
              >
                <option value="">{t("naming.digitsNone")}</option>
                {COUNTER_DIGITS.map((d) => (
                  <option key={d} value={d}>
                    {d === "auto" ? t("naming.digitsAuto") : d}
                  </option>
                ))}
              </select>
            </label>
          </div>
        )}
        <div className="naming-example" id={`${id}-example`} aria-live="polite">
          <span>{t(folder ? "naming.exampleFolder" : "naming.example")}</span>
          <code>{example || t("naming.noFolder")}</code>
          {preview?.dateSource && valid && (
            <small className="naming-date-source">
              {t(`naming.dateSources.${preview.dateSource}`)}
              {preview.sidecar ? ` · ${t("naming.sidecarCopied")}` : ""}
            </small>
          )}
        </div>
        <details className="naming-builder">
          <summary>{t(folder ? "naming.editFolder" : "naming.edit")}</summary>
          <p>{t("naming.hint")}</p>
          {NAMING_GROUPS.map((group) => {
            const items = vocabulary.filter((token) => token.group === group);
            if (!items.length) return null;
            return (
              <fieldset key={group} className="naming-group">
                <legend>{t(`naming.groups.${group}`)}</legend>
                <div className="naming-tokens">
                  {items.map((token) => (
                    <NamingDragButton
                      key={token.value}
                      id={`token-${token.value}`}
                      data={{ kind: "token", text: token.value }}
                      title={`${token.value} · ${t("naming.addHint")}`}
                      onDoubleClick={() => add(token.value)}
                      onKeyDown={listKeys(token.value)}
                    >
                      {tokenName(token.value)}
                    </NamingDragButton>
                  ))}
                </div>
              </fieldset>
            );
          })}
          <fieldset className="naming-group">
            <legend>{t("naming.groups.text")}</legend>
            <div className="naming-tokens">
              {separators.map((separator) => (
                <NamingDragButton
                  key={separator}
                  id={`separator-${separator}`}
                  data={{ kind: "token", text: separator }}
                  title={t("naming.addHint")}
                  onDoubleClick={() => add(separator)}
                  onKeyDown={listKeys(separator)}
                >
                  {separator === " " ? t("naming.space") : separator === "/" ? t("naming.folderLevel") : separator}
                </NamingDragButton>
              ))}
            </div>
          </fieldset>
          <label className="naming-code" htmlFor={id}>
            {label}
            <input
              id={id}
              ref={code}
              value={value}
              required={!folder}
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
              aria-invalid={!valid}
              onChange={(e) => {
                onChange(e.target.value);
                caret.current = null;
              }}
            />
          </label>
          {!folder && <small>{t("naming.dateNote")}</small>}
          {!folder && (
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
                disabled={!name.trim() || !valid || !validSequenceStart(sequenceStart) || saved.length >= 50}
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
              {custom && (
                <button
                  type="button"
                  disabled={!name.trim() || name.trim() === custom.name || saved.some((p) => p.name === name.trim())}
                  onClick={() => {
                    persist(saved.map((p, i) => (i === custom.index ? { ...p, name: name.trim() } : p)));
                    setName("");
                  }}
                >
                  {t("naming.renamePreset")}
                </button>
              )}
              {custom && (
                <button type="button" onClick={() => persist(saved.filter((_, i) => i !== custom.index))}>
                  {t("naming.deletePreset")}
                </button>
              )}
            </div>
          )}
          {storageError && <p role="alert">{t("naming.saveError")}</p>}
        </details>
        <DragOverlay dropAnimation={null}>
          {dragging && <span className="naming-drag-preview">{tokenName(dragging.text)}</span>}
        </DragOverlay>
      </DndContext>
    </div>
  );
}

/** The choice a chip offers: the counter's digits, the date's or time's
 * format. */
function ChipOption({ part, onOption }: { part: string; onOption: (option: string | undefined) => void }) {
  const { t } = useTranslation();
  const { name, option } = parseToken(part);
  if (name === "seq")
    return (
      <select
        className="naming-chip-option"
        aria-label={t("naming.digits")}
        value={option ?? ""}
        onChange={(e) => onOption(e.target.value || undefined)}
      >
        <option value="">{t("naming.digitsNone")}</option>
        {COUNTER_DIGITS.map((d) => (
          <option key={d} value={d}>
            {d === "auto" ? t("naming.digitsAuto") : d}
          </option>
        ))}
        {option && !(COUNTER_DIGITS as readonly string[]).includes(option) && <option value={option}>{option}</option>}
      </select>
    );
  if (name === "date" || name === "time") {
    const formats: readonly string[] = name === "date" ? DATE_FORMATS : TIME_FORMATS;
    return (
      <select
        className="naming-chip-option"
        aria-label={t(name === "date" ? "naming.dateFormat" : "naming.timeFormat")}
        value={option ?? formats[0]}
        onChange={(e) => onOption(e.target.value === formats[0] ? undefined : e.target.value)}
      >
        {formats.map((format) => (
          <option key={format} value={format}>
            {format.toUpperCase()}
          </option>
        ))}
      </select>
    );
  }
  return null;
}
