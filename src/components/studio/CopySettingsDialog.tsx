import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import type { ParseKeys } from "i18next";
import { create } from "zustand";
import type { Adjustments, MaskContainer } from "../../utils/adjustments";
import {
  DETECTED_TYPES,
  RESPONSES_CHOSEN,
  RESPONSE_KEYS,
  SETTINGS_GROUPS,
  groupModified,
  type SettingsGroup,
} from "../../utils/settingsClipboard";
import type { SubMask } from "../panel/right/Masks";

/** What the picker chooses from: the source photograph's settings. */
export interface SettingsSource {
  path: string;
  adjustments: Adjustments;
  isRaw: boolean;
}

/** The choice made: settings keys of the chosen groups, chosen layer ids. */
export interface SettingsChoice {
  keys: string[];
  layerIds: string[];
}

interface PickerRequest {
  /** Copy to the clipboard, or sync to `targets` now. */
  mode: "copy" | "sync";
  source: SettingsSource;
  targets: string[];
  /** The keys chosen last time (remembered in the preferences). */
  remembered: string[];
  /** The source's layers chosen last time. */
  rememberedLayers: string[];
  onChoose(choice: SettingsChoice): void;
}

interface PickerState {
  request: PickerRequest | null;
  open(request: PickerRequest): void;
  close(): void;
}

export const useCopySettingsDialog = create<PickerState>((set) => ({
  request: null,
  open: (request) => set({ request }),
  close: () => set({ request: null }),
}));

const SECTIONS = [...new Set(SETTINGS_GROUPS.map((group) => group.section))];

/** The groups `keys` choose: any of a group's keys chosen chooses it. */
export function chosenGroups(keys: string[]): Set<string> {
  const chosen = new Set(keys);
  return new Set(SETTINGS_GROUPS.filter((group) => group.keys.some((key) => chosen.has(key))).map((g) => g.label));
}

/** The keys of the chosen groups, `masks` when layers are chosen, the
 * processing versions (and their mark) when they are. */
export function keysOf(groups: Set<string>, withLayers: boolean, withResponses = false): string[] {
  const keys = SETTINGS_GROUPS.filter((group) => groups.has(group.label)).flatMap((group) => group.keys);
  return [...keys, ...(withLayers ? ["masks"] : []), ...(withResponses ? [...RESPONSE_KEYS, RESPONSES_CHOSEN] : [])];
}

const layerName = (layer: MaskContainer, index: number) => layer.name?.trim() || `#${index + 1}`;

export function CopySettingsDialogHost() {
  const request = useCopySettingsDialog((s) => s.request);
  if (!request) return null;
  return <CopySettingsDialog key={`${request.mode}:${request.source.path}`} request={request} />;
}

/**
 * Copy and Sync settings: which tools and which local layers, the better of
 * Lightroom's Copy/Sync Settings dialog (every setting by group, remembered)
 * and Capture One's adjustments clipboard (adjusted tools chosen at a click,
 * layers one by one). Retouching, AI patches (generated pixels), Relight and
 * lens blur belong to their photograph and are never copied.
 */
function CopySettingsDialog({ request }: { request: PickerRequest }) {
  const { t } = useTranslation();
  const close = useCopySettingsDialog((s) => s.close);
  const dialog = useRef<HTMLDivElement>(null);
  // The keyboard goes to the picker while it is open and back after.
  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    dialog.current?.focus();
    return () => before?.focus?.();
  }, []);
  const { source } = request;
  const layers = source.adjustments.masks ?? [];
  const modified = useMemo(
    () => new Set(SETTINGS_GROUPS.filter((g) => groupModified(g, source.adjustments, source.isRaw)).map((g) => g.label)),
    [source],
  );
  const [groups, setGroups] = useState<Set<string>>(() => chosenGroups(request.remembered));
  const [layerIds, setLayerIds] = useState<Set<string>>(() => new Set(request.rememberedLayers));
  // Off unless chosen last time (Lightroom's Process Version).
  const [responses, setResponses] = useState(() => request.remembered.includes(RESPONSES_CHOSEN));
  const toggle = (set: Set<string>, value: string, on: boolean) => {
    const next = new Set(set);
    if (on) next.add(value);
    else next.delete(value);
    return next;
  };
  const sectionGroups = (section: string) => SETTINGS_GROUPS.filter((group) => group.section === section);
  const choose = () => {
    request.onChoose({ keys: keysOf(groups, layerIds.size > 0, responses), layerIds: [...layerIds] });
    close();
  };
  const keys = (e: KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Escape") close();
  };
  const title =
    request.mode === "copy"
      ? t("studio.copySettings.copyTitle")
      : t("studio.copySettings.syncTitle", { n: request.targets.length });
  const nothing = groups.size === 0 && layerIds.size === 0 && !responses;
  return createPortal(
    <div className="studio-settings studio-smart-album-backdrop" onKeyDown={keys}>
      <div
        ref={dialog}
        className="studio-copy-settings"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onKeyDown={(e) => {
          // Tab stays inside the picker.
          if (e.key !== "Tab") return;
          const controls = [...e.currentTarget.querySelectorAll<HTMLElement>("button:not(:disabled), input")];
          if (controls.length === 0) return;
          const [first, last] = [controls[0], controls[controls.length - 1]];
          if (e.shiftKey && (document.activeElement === first || document.activeElement === e.currentTarget)) {
            e.preventDefault();
            last.focus();
          } else if (!e.shiftKey && document.activeElement === last) {
            e.preventDefault();
            first.focus();
          }
        }}
      >
        <h2>{title}</h2>
        <div className="studio-copy-settings-quick">
          <button
            type="button"
            className="studio-button"
            onClick={() => {
              setGroups(new Set(SETTINGS_GROUPS.map((g) => g.label)));
              setLayerIds(new Set(layers.map((layer) => layer.id)));
            }}
          >
            {t("studio.copySettings.all")}
          </button>
          <button
            type="button"
            className="studio-button"
            onClick={() => (setGroups(new Set()), setLayerIds(new Set()), setResponses(false))}
          >
            {t("studio.copySettings.none")}
          </button>
          <button
            type="button"
            className="studio-button"
            title={t("studio.copySettings.modifiedTitle")}
            onClick={() => {
              setGroups(new Set(modified));
              setLayerIds(new Set(layers.map((layer) => layer.id)));
            }}
          >
            {t("studio.copySettings.modified")}
          </button>
        </div>
        <div className="studio-copy-settings-groups">
          {SECTIONS.map((section) => {
            const inSection = sectionGroups(section);
            const on = inSection.filter((group) => groups.has(group.label)).length;
            return (
              <fieldset key={section}>
                <legend>
                  <label>
                    <input
                      type="checkbox"
                      checked={on === inSection.length}
                      ref={(input) => {
                        if (input) input.indeterminate = on > 0 && on < inSection.length;
                      }}
                      onChange={(e) =>
                        setGroups(
                          inSection.reduce((set, group) => toggle(set, group.label, e.target.checked), groups),
                        )
                      }
                    />
                    {t(`studio.copySettings.sections.${section}` as ParseKeys)}
                  </label>
                </legend>
                {inSection.map((group: SettingsGroup) => (
                  <label key={group.label} className={modified.has(group.label) ? "is-modified" : undefined}>
                    <input
                      type="checkbox"
                      checked={groups.has(group.label)}
                      onChange={(e) => setGroups(toggle(groups, group.label, e.target.checked))}
                    />
                    {t(group.label as ParseKeys)}
                    {modified.has(group.label) && (
                      <span className="studio-copy-settings-mark" title={t("studio.copySettings.changed")}>
                        •
                      </span>
                    )}
                  </label>
                ))}
              </fieldset>
            );
          })}
          <fieldset>
            <legend>{t("studio.copySettings.versions")}</legend>
            <label>
              <input type="checkbox" checked={responses} onChange={(e) => setResponses(e.target.checked)} />
              {t("studio.copySettings.versionsChoice")}
            </label>
            <p className="studio-help">{t("studio.copySettings.versionsNote")}</p>
          </fieldset>
          <fieldset>
            <legend>{t("studio.copySettings.layers")}</legend>
            {layers.length === 0 && <p className="studio-help">{t("studio.copySettings.noLayers")}</p>}
            {layers.map((layer, index) => {
              const detected = layer.subMasks.some((sub: SubMask) => DETECTED_TYPES.has(sub.type));
              return (
                <label key={layer.id}>
                  <input
                    type="checkbox"
                    checked={layerIds.has(layer.id)}
                    onChange={(e) => setLayerIds(toggle(layerIds, layer.id, e.target.checked))}
                  />
                  {layerName(layer, index)}
                  {detected && (
                    <>
                      {" "}
                      <span className="studio-copy-settings-note">{t("studio.copySettings.detectedHere")}</span>
                    </>
                  )}
                </label>
              );
            })}
          </fieldset>
        </div>
        <p className="studio-help">{t("studio.copySettings.rules")}</p>
        <footer>
          <button className="studio-button" onClick={close}>
            {t("studio.copySettings.cancel")}
          </button>
          <button className="studio-button is-primary" disabled={nothing} onClick={choose}>
            {request.mode === "copy"
              ? t("studio.copySettings.copy")
              : t("studio.copySettings.sync", { n: request.targets.length })}
          </button>
        </footer>
      </div>
    </div>,
    document.body,
  );
}
