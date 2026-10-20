import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import type { ParseKeys } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { toast } from "react-toastify";
import { Plus, X } from "lucide-react";
import { create } from "zustand";
import { useModalFocus } from "../../hooks/useModalFocus";
import { useLibraryStore } from "../../store/useLibraryStore";
import { Invokes } from "../ui/AppProperties";
import {
  isGroup,
  searchLibrary,
  type Combine,
  type Condition,
  type Field,
  type Group,
  type Op,
  type Rule,
} from "../../utils/librarySearch";
import { SESSION_ALBUMS_CHANGED, childrenOf, type SessionAlbum } from "./StudioAlbums";

/** What a field's value is, which decides its operators and inputs. */
type Kind = "search" | "text" | "number" | "date" | "choice" | "album" | "folder" | "bool";

const FIELDS: { field: Field; kind: Kind }[] = [
  { field: "text", kind: "search" },
  { field: "details", kind: "search" },
  { field: "filename", kind: "text" },
  { field: "keyword", kind: "text" },
  { field: "rating", kind: "number" },
  { field: "flag", kind: "choice" },
  { field: "label", kind: "choice" },
  { field: "fileType", kind: "choice" },
  { field: "camera", kind: "text" },
  { field: "make", kind: "text" },
  { field: "lens", kind: "text" },
  { field: "iso", kind: "number" },
  { field: "focalLength", kind: "number" },
  { field: "aperture", kind: "number" },
  { field: "exposure", kind: "number" },
  { field: "captureDate", kind: "date" },
  { field: "importDate", kind: "date" },
  { field: "album", kind: "album" },
  { field: "folder", kind: "folder" },
  { field: "offline", kind: "bool" },
  { field: "previousImport", kind: "bool" },
];
const kindOf = (field: Field): Kind => FIELDS.find((f) => f.field === field)?.kind ?? "text";

export const OPS: Record<Kind, Op[]> = {
  search: ["contains", "containsAny", "notContains"],
  text: ["contains", "containsAny", "notContains", "is", "isNot", "startsWith", "endsWith", "empty", "notEmpty"],
  number: ["is", "isNot", "gt", "ge", "lt", "le", "between", "empty", "notEmpty"],
  date: ["is", "isNot", "lt", "gt", "between", "inLast", "notInLast", "empty", "notEmpty"],
  choice: ["is", "isNot"],
  album: ["is", "isNot"],
  folder: ["contains", "is", "notContains", "isNot"],
  bool: ["is"],
};

export const CHOICES: Partial<Record<Field, string[]>> = {
  flag: ["picked", "unflagged", "rejected"],
  label: ["red", "yellow", "green", "blue", "purple", "none"],
  fileType: ["raw", "dng", "jpeg", "heif", "tiff", "png", "other"],
};

const today = () => {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
};

/** Exposure as typed: `1/250`, `0.5`, `2`, `2s` → seconds (NaN if not). */
export function parseExposure(text: string): number {
  const clean = text.trim().replace(/s$/i, "").trim();
  const [n, d] = clean.split("/");
  if (d !== undefined) return Number(n) / Number(d);
  return Number(clean);
}

/** Seconds as photographers write them where that is exact (1/250, 2),
 * else as the number itself (0.625): what is shown is what is saved. */
export function formatExposure(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "";
  const denominator = Math.round(1 / seconds);
  if (seconds < 1 && denominator > 1 && 1 / denominator === seconds) return `1/${denominator}`;
  return String(seconds);
}

/** A fresh value for `op` of `field` (keeps `previous` when it still fits). */
function valueFor(field: Field, op: Op, previous: unknown, albums: SessionAlbum[], folder: string | null): unknown {
  const kind = kindOf(field);
  if (op === "empty" || op === "notEmpty") return undefined;
  if (op === "inLast" || op === "notInLast")
    return typeof previous === "object" && previous && "amount" in previous ? previous : { amount: 30, unit: "days" };
  if (op === "between") {
    if (Array.isArray(previous) && previous.length === 2) return previous;
    if (kind === "date") return [today(), today()];
    return field === "rating" ? [1, 5] : [0, 0];
  }
  switch (kind) {
    case "search":
    case "text":
      return typeof previous === "string" ? previous : "";
    case "number":
      if (typeof previous === "number") return previous;
      return field === "rating" ? 3 : field === "exposure" ? 1 / 250 : 0;
    case "date":
      return typeof previous === "string" ? previous : today();
    case "choice":
      return Array.isArray(previous) || typeof previous === "string" ? previous : [CHOICES[field]?.[0]];
    case "album":
      return typeof previous === "string" ? previous : (albums.find((a) => a.kind !== "group")?.id ?? "");
    case "folder":
      return typeof previous === "string" ? previous : (folder ?? "");
    case "bool":
      return typeof previous === "boolean" ? previous : true;
  }
}

/** What the editor is asked to show: a new smart album (in `parent`) or an
 * existing one (`id`). */
export interface SmartAlbumDraft {
  id?: string;
  name: string;
  parent: string | null;
  rules: Group;
}

interface EditorState {
  draft: SmartAlbumDraft | null;
  open: (draft: SmartAlbumDraft) => void;
  close: () => void;
}

/** Opens the smart album editor from anywhere (Albums tool, filter bar). */
export const useSmartAlbumEditor = create<EditorState>((set) => ({
  draft: null,
  open: (draft) => set({ draft }),
  close: () => set({ draft: null }),
}));

/** A group as the editor edits it: a rule given alone becomes "all of it". */
export const asGroup = (rule: Rule): Group => (isGroup(rule) ? rule : { combine: "all", rules: [rule] });

interface RuleProps {
  rule: Rule;
  albums: SessionAlbum[];
  folder: string | null;
  depth: number;
  onChange: (rule: Rule) => void;
  onRemove?: () => void;
}

function GroupEditor({ rule, albums, folder, depth, onChange, onRemove }: RuleProps & { rule: Group }) {
  const { t } = useTranslation();
  const set = (index: number, next: Rule) =>
    onChange({ ...rule, rules: rule.rules.map((r, i) => (i === index ? next : r)) });
  const remove = (index: number) => onChange({ ...rule, rules: rule.rules.filter((_, i) => i !== index) });
  const fresh: Condition = { field: "rating", op: "ge", value: 3 };
  return (
    <div className={`studio-rule-group ${depth > 0 ? "is-nested" : ""}`} role="group">
      <div className="studio-rule-group-head">
        <label>
          <span>{t("studio.library.smartAlbums.match")}</span>
          <select
            aria-label={t("studio.library.smartAlbums.combineLabel")}
            value={rule.combine}
            onChange={(e) => onChange({ ...rule, combine: e.target.value as Combine })}
          >
            {(["all", "any", "none"] as const).map((combine) => (
              <option key={combine} value={combine}>
                {t(`studio.library.smartAlbums.combine.${combine}`)}
              </option>
            ))}
          </select>
          <span>{t("studio.library.smartAlbums.ofTheseRules")}</span>
        </label>
        {onRemove && (
          <button className="studio-rule-remove" aria-label={t("studio.library.smartAlbums.removeGroup")} onClick={onRemove}>
            <X size={12} />
          </button>
        )}
      </div>
      {rule.rules.map((child, index) =>
        isGroup(child) ? (
          <GroupEditor
            key={index}
            rule={child}
            albums={albums}
            folder={folder}
            depth={depth + 1}
            onChange={(next) => set(index, next)}
            onRemove={() => remove(index)}
          />
        ) : (
          <ConditionEditor
            key={index}
            rule={child}
            albums={albums}
            folder={folder}
            depth={depth + 1}
            onChange={(next) => set(index, next)}
            onRemove={() => remove(index)}
          />
        ),
      )}
      <div className="studio-rule-add">
        <button onClick={() => onChange({ ...rule, rules: [...rule.rules, fresh] })}>
          <Plus size={11} /> {t("studio.library.smartAlbums.addRule")}
        </button>
        {depth < 6 && (
          <button onClick={() => onChange({ ...rule, rules: [...rule.rules, { combine: "any", rules: [fresh] }] })}>
            <Plus size={11} /> {t("studio.library.smartAlbums.addGroup")}
          </button>
        )}
      </div>
    </div>
  );
}

function ConditionEditor({ rule, albums, folder, onChange, onRemove }: RuleProps & { rule: Condition }) {
  const { t } = useTranslation();
  const kind = kindOf(rule.field);
  const ops = OPS[kind];
  const setField = (field: Field) => {
    const nextKind = kindOf(field);
    const op = OPS[nextKind].includes(rule.op) ? rule.op : OPS[nextKind][0];
    const keep = nextKind === kind && field !== "rating" && rule.field !== "rating" ? rule.value : undefined;
    onChange({ field, op, value: valueFor(field, op, nextKind === "choice" ? undefined : keep, albums, folder) });
  };
  const setOp = (op: Op) => onChange({ ...rule, op, value: valueFor(rule.field, op, rule.value, albums, folder) });
  const setValue = (value: unknown) => onChange({ ...rule, value });
  const label = t(`studio.library.smartAlbums.fields.${rule.field}`);
  return (
    <div className="studio-rule" role="group" aria-label={label}>
      <select aria-label={t("studio.library.smartAlbums.fieldLabel")} value={rule.field} onChange={(e) => setField(e.target.value as Field)}>
        {FIELDS.map(({ field }) => (
          <option key={field} value={field}>
            {t(`studio.library.smartAlbums.fields.${field}`)}
          </option>
        ))}
      </select>
      <select aria-label={t("studio.library.smartAlbums.opLabel")} value={rule.op} onChange={(e) => setOp(e.target.value as Op)}>
        {ops.map((op) => (
          <option key={op} value={op}>
            {t(`studio.library.smartAlbums.ops.${kind}.${op}` as ParseKeys)}
          </option>
        ))}
      </select>
      <ValueEditor rule={rule} kind={kind} albums={albums} onChange={setValue} />
      <button className="studio-rule-remove" aria-label={t("studio.library.smartAlbums.removeRule", { field: label })} onClick={onRemove}>
        <X size={12} />
      </button>
    </div>
  );
}

function NumberInput({ field, value, onChange, label }: { field: Field; value: unknown; onChange: (v: number) => void; label: string }) {
  const shown = (v: unknown) => (field === "exposure" ? formatExposure(Number(v)) : String(v ?? ""));
  const [text, setText] = useState(() => shown(value));
  // The rule's value changed from outside the typing (a rule above was
  // removed and this row now shows another rule): show it. While typing,
  // the value follows the text, so the text is kept as typed ("1/").
  const parse = (t: string) => (t.trim() === "" ? NaN : field === "exposure" ? parseExposure(t) : Number(t));
  const typed = useRef(value);
  if (!Object.is(typed.current, value)) {
    typed.current = value;
    if (!Object.is(parse(text), value)) setText(shown(value));
  }
  return (
    <input
      className="studio-metadata-input"
      aria-label={label}
      inputMode="decimal"
      value={text}
      onChange={(e) => {
        setText(e.target.value);
        // Text that is no number (yet: "1/", "abc") makes the rule
        // incomplete, so what is saved is always what is shown.
        const n = parse(e.target.value);
        typed.current = n;
        onChange(n);
      }}
    />
  );
}

function ValueEditor({
  rule,
  kind,
  albums,
  onChange,
}: {
  rule: Condition;
  kind: Kind;
  albums: SessionAlbum[];
  onChange: (value: unknown) => void;
}) {
  const { t } = useTranslation();
  const { field, op, value } = rule;
  const label = t("studio.library.smartAlbums.valueLabel");
  if (op === "empty" || op === "notEmpty") return null;
  if (op === "inLast" || op === "notInLast") {
    const period = (value ?? {}) as { amount?: number; unit?: string };
    return (
      <span className="studio-rule-value">
        <input
          className="studio-metadata-input"
          aria-label={t("studio.library.smartAlbums.amountLabel")}
          type="number"
          min={1}
          value={period.amount ?? 1}
          onChange={(e) => onChange({ ...period, amount: Math.max(1, Math.round(Number(e.target.value) || 1)) })}
        />
        <select
          aria-label={t("studio.library.smartAlbums.unitLabel")}
          value={period.unit ?? "days"}
          onChange={(e) => onChange({ ...period, unit: e.target.value })}
        >
          {["days", "weeks", "months", "years"].map((unit) => (
            <option key={unit} value={unit}>
              {t(`studio.library.smartAlbums.units.${unit}` as ParseKeys)}
            </option>
          ))}
        </select>
      </span>
    );
  }
  if (op === "between") {
    const [from, to] = (Array.isArray(value) ? value : [undefined, undefined]) as unknown[];
    const one = (v: unknown, set: (v: unknown) => void, key: string) =>
      kind === "date" ? (
        <input key={key} className="studio-metadata-input" type="date" aria-label={label} value={String(v ?? "")} onChange={(e) => set(e.target.value)} />
      ) : (
        <NumberInput key={key} field={field} value={v} label={label} onChange={set} />
      );
    return (
      <span className="studio-rule-value">
        {one(from, (v) => onChange([v, to]), "from")}
        <span>{t("studio.library.smartAlbums.and")}</span>
        {one(to, (v) => onChange([from, v]), "to")}
      </span>
    );
  }
  switch (kind) {
    case "search":
    case "text":
    case "folder":
      return (
        <input
          className="studio-metadata-input studio-rule-text"
          aria-label={label}
          value={String(value ?? "")}
          placeholder={kind === "folder" ? t("studio.library.smartAlbums.folderPlaceholder") : undefined}
          onChange={(e) => onChange(e.target.value)}
        />
      );
    case "number":
      return <NumberInput key={`${field}-${op}`} field={field} value={value} label={label} onChange={onChange} />;
    case "date":
      return <input className="studio-metadata-input" type="date" aria-label={label} value={String(value ?? "")} onChange={(e) => onChange(e.target.value)} />;
    case "bool":
      return (
        <select aria-label={label} value={value === false ? "no" : "yes"} onChange={(e) => onChange(e.target.value === "yes")}>
          <option value="yes">{t("studio.library.smartAlbums.yes")}</option>
          <option value="no">{t("studio.library.smartAlbums.no")}</option>
        </select>
      );
    case "album": {
      const options: { album: SessionAlbum; depth: number }[] = [];
      const walk = (parent: string | null, depth: number) => {
        for (const album of childrenOf(albums, parent)) {
          options.push({ album, depth });
          walk(album.id, depth + 1);
        }
      };
      walk(null, 0);
      return (
        <select aria-label={label} value={String(value ?? "")} onChange={(e) => onChange(e.target.value)}>
          {options.map(({ album, depth }) => (
            <option key={album.id} value={album.id}>
              {`${"  ".repeat(depth)}${album.name}`}
            </option>
          ))}
        </select>
      );
    }
    case "choice": {
      const chosen = Array.isArray(value) ? (value as string[]) : typeof value === "string" ? [value] : [];
      return (
        <span className="studio-rule-value studio-rule-choices" role="group" aria-label={label}>
          {(CHOICES[field] ?? []).map((choice) => (
            <label key={choice}>
              <input
                type="checkbox"
                checked={chosen.includes(choice)}
                onChange={(e) =>
                  onChange(e.target.checked ? [...chosen, choice] : chosen.filter((c) => c !== choice))
                }
              />
              {t(`studio.library.smartAlbums.choices.${field}.${choice}` as ParseKeys)}
            </label>
          ))}
        </span>
      );
    }
  }
}

/** Rules the catalog will refuse that the editor can tell before saving. */
export function incomplete(rule: Rule): boolean {
  if (isGroup(rule)) return rule.rules.some(incomplete);
  const kind = kindOf(rule.field);
  if (kind === "choice")
    return Array.isArray(rule.value) ? rule.value.length === 0 : typeof rule.value !== "string" || !rule.value;
  if (kind === "album" || kind === "folder") return !rule.value;
  if (rule.op === "between" && Array.isArray(rule.value)) {
    if (kind === "number") return rule.value.some((v) => typeof v !== "number" || !Number.isFinite(v));
    return rule.value.some((v) => v === "" || v === undefined);
  }
  if (kind === "number" && rule.op !== "empty" && rule.op !== "notEmpty")
    return typeof rule.value !== "number" || !Number.isFinite(rule.value);
  return false;
}

/**
 * The smart album editor: a name and rules (Lightroom's smart collection
 * rules and Capture One's smart album criteria as one tool: every field,
 * groups matching all, any or none of their rules, nested), with how many
 * photographs they match now. Saved in the library; the album stays
 * current by itself.
 */
export function SmartAlbumEditorHost({ albums }: { albums?: SessionAlbum[] }) {
  const draft = useSmartAlbumEditor((s) => s.draft);
  if (!draft) return null;
  return <SmartAlbumEditor key={draft.id ?? "new"} draft={draft} albums={albums} />;
}

function SmartAlbumEditor({ draft, albums: given }: { draft: SmartAlbumDraft; albums?: SessionAlbum[] }) {
  const { t } = useTranslation();
  const close = useSmartAlbumEditor((s) => s.close);
  const directory = useLibraryStore((s) => s.sessionDirectory);
  const folder = useLibraryStore((s) => s.currentFolderPath);
  const [name, setName] = useState(draft.name);
  const [rules, setRules] = useState<Group>(draft.rules);
  const [albums, setAlbums] = useState<SessionAlbum[]>(given ?? []);
  const [matches, setMatches] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  useModalFocus(ref, true);

  useEffect(() => {
    if (given || !directory) return;
    invoke<SessionAlbum[]>(Invokes.SessionAlbums, { directory })
      .then((list) => setAlbums((list ?? []).filter((a) => a.id !== draft.id)))
      .catch(() => {});
  }, [given, directory, draft.id]);

  // How many photographs the rules match now (as the album will show).
  useEffect(() => {
    if (!directory || incomplete(rules)) return setMatches(null);
    let cancelled = false;
    const timer = setTimeout(() => {
      searchLibrary(directory, rules)
        .then((paths) => {
          if (cancelled) return;
          setMatches(paths.length);
          setError(null);
        })
        .catch((e) => {
          if (cancelled) return;
          setMatches(null);
          setError(String(e));
        });
    }, 150);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [directory, rules]);

  const save = async () => {
    if (!directory || !name.trim()) return;
    try {
      if (draft.id) {
        await invoke(Invokes.SessionSetSmartAlbumRules, { directory, id: draft.id, rules });
        if (name.trim() !== draft.name)
          await invoke(Invokes.SessionRenameAlbum, { directory, id: draft.id, name: name.trim() });
      } else {
        await invoke(Invokes.SessionCreateSmartAlbum, { directory, name: name.trim(), parent: draft.parent, rules });
      }
      window.dispatchEvent(new Event(SESSION_ALBUMS_CHANGED));
      close();
    } catch (e) {
      toast.error(t("studio.library.smartAlbums.saveFailed", { error: String(e) }));
    }
  };
  const keys = (e: KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Escape") close();
  };
  const title = draft.id ? t("studio.library.smartAlbums.editTitle") : t("studio.library.smartAlbums.newTitle");
  return createPortal(
    <div className="studio-settings studio-smart-album-backdrop" onKeyDown={keys}>
      <div ref={ref} className="studio-smart-album" role="dialog" aria-modal="true" aria-label={title} tabIndex={-1}>
        <h2>{title}</h2>
        <label className="studio-smart-album-name">
          <span>{t("studio.library.smartAlbums.name")}</span>
          <input className="studio-metadata-input" value={name} autoFocus onChange={(e) => setName(e.target.value)} />
        </label>
        <div className="studio-smart-album-rules">
          <GroupEditor rule={rules} albums={albums} folder={folder} depth={0} onChange={(next) => setRules(asGroup(next))} />
        </div>
        <footer>
          <span className="studio-smart-album-count" role="status">
            {error
              ? error
              : matches === null
                ? t("studio.library.smartAlbums.incomplete")
                : t("studio.library.smartAlbums.matches", { count: matches, n: matches.toLocaleString() })}
          </span>
          <button onClick={close}>{t("studio.library.smartAlbums.cancel")}</button>
          <button className="is-primary" disabled={!name.trim() || incomplete(rules) || !!error} onClick={() => void save()}>
            {draft.id ? t("studio.library.smartAlbums.save") : t("studio.library.smartAlbums.create")}
          </button>
        </footer>
      </div>
    </div>,
    document.body,
  );
}
