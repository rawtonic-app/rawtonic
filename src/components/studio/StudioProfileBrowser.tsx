import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useReducer, useRef, useState, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { LayoutGrid, List, Star } from "lucide-react";
import { Invokes } from "../ui/AppProperties";
import OperationError from "../ui/OperationError";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { loadBuiltinPresets, presetName, useBuiltinPresets } from "../../utils/builtinPresets";
import type { Adjustments } from "../../utils/adjustments";
import AdjustmentControl from "./AdjustmentControl";
import { reportPerformance } from "../../hooks/usePerformanceDiagnostics";

interface LutEntry {
  name: string;
  path: string;
  isBuiltIn: boolean;
  group?: string | null;
}

/** A camera profile of the photo's camera (`list_camera_profiles`): an
 * imported DCP, copied into the session's `profiles/` folder. */
export interface CameraProfileEntry {
  value: string;
  name: string;
  camera: string;
}

export type ProfileKind = "camera" | "look" | "bw";

/** One choice of the browser: what it renders and applies. */
export interface ProfileTile {
  /** `camera:<profile>`, `look:builtin:<name>`, `look:<imported path>` or
   * `bw:<mix id>` (favourites). */
  key: string;
  kind: ProfileKind;
  /** The group it is listed in (translated, or an imported pack's name). */
  group: string;
  name: string;
  /** The recipe keys it sets; null removes a key. */
  variant: Record<string, unknown>;
  selected: boolean;
  lut?: LutEntry;
}

// Built-in looks: file names are recipe identifiers, the names shown our own.
const LOOK_NAMES: Record<string, string> = {
  "Portrait 400": "softPortrait",
  "Cinema 250D": "warmCinema",
  "Cinema 500T": "coolCinema",
  "Pastel 400": "pastel",
  "Vivid 100": "vivid",
  "Warm 200": "warm",
};

const TILE_POINTS = 160;
/** Tiles per native call: the first rows arrive while later ones render. */
const BATCH = 8;

const sameMixer = (a: unknown, b: unknown) =>
  !!a && !!b && Object.entries(b as Record<string, number>).every(([k, v]) => (a as Record<string, number>)[k] === v);

/** A tile's keys over `adjustments`: a look turns the Effects section on,
 * as applying it does (`handleLutSelect`), so tile, preview and result
 * agree. */
export function tileKeys(tile: Pick<ProfileTile, "kind" | "variant">, adjustments: Adjustments): Record<string, unknown> {
  if (tile.kind !== "look" || !tile.variant.lutPath) return tile.variant;
  return { ...tile.variant, sectionVisibility: { ...adjustments.sectionVisibility, effects: true } };
}

/** The recipe with a tile's keys (the hover preview). */
export function withVariant(adjustments: Adjustments, variant: Record<string, unknown>): Adjustments {
  const out = { ...adjustments } as Record<string, unknown>;
  for (const [key, value] of Object.entries(variant)) {
    if (value === null && key === "cameraProfile") delete out[key];
    else out[key] = value;
  }
  return out as unknown as Adjustments;
}

/** Splits `profile_thumbnails`' answer (u32 LE length + JPEG per tile). */
export function splitTiles(buffer: ArrayBuffer, count: number): (Blob | null)[] {
  const view = new DataView(buffer);
  const out: (Blob | null)[] = [];
  let at = 0;
  for (let i = 0; i < count && at + 4 <= buffer.byteLength; i++) {
    const length = view.getUint32(at, true);
    at += 4;
    out.push(length > 0 ? new Blob([buffer.slice(at, at + length)], { type: "image/jpeg" }) : null);
    at += length;
  }
  return out;
}

let generations = 0;

/** Lightroom's profile browser (its better form than a list, UXR3 D4): the
 * open photograph rendered small with each camera profile, look and
 * black-and-white mix, in groups with the favourites first. Hovering or
 * moving the focus onto one shows it in the main view (the renderer draws
 * the override; nothing is applied, Esc or leaving restores); a click or
 * Enter applies it as one history step. Looks have an Amount (0–200 %,
 * the recipe's `lutIntensity`); camera profiles and mixes have none. */
export default function StudioProfileBrowser({
  disabled,
  onInteraction,
}: {
  disabled: boolean;
  onInteraction(active: boolean): void;
}) {
  const { t } = useTranslation();
  const image = useEditorStore((s) => s.selectedImage);
  const imagePath = image?.path;
  const isRaw = Boolean(image?.isRaw);
  const cameraProfile = useEditorStore((s) => s.adjustments.cameraProfile);
  const lutPath = useEditorStore((s) => s.adjustments.lutPath ?? null);
  const intensity = useEditorStore((s) => s.adjustments.lutIntensity ?? 100);
  const blackAndWhite = useEditorStore((s) => Boolean(s.adjustments.blackAndWhite));
  const grayMixer = useEditorStore((s) => s.adjustments.grayMixer);
  const { handleLutSelect, setAdjustments } = useEditorActions();
  const favourites = useSettingsStore((s) => s.appSettings?.favoriteProfiles);
  const showBuiltins = useSettingsStore((s) => s.appSettings?.showBuiltinPresets !== false);
  const builtins = useBuiltinPresets((s) => s.presets);

  const [opened, setOpened] = useState(false);
  const [layout, setLayout] = useState<"grid" | "list">("grid");
  const [query, setQuery] = useState("");
  const [profiles, setProfiles] = useState<CameraProfileEntry[]>([]);
  const [luts, setLuts] = useState<LutEntry[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => void loadBuiltinPresets(), []);
  useEffect(() => {
    let current = true;
    invoke<LutEntry[]>(Invokes.ListLuts)
      .then((list) => current && setLuts(Array.isArray(list) ? list : []))
      .catch(() => current && setLuts([]));
    return () => {
      current = false;
    };
  }, [opened]);
  useEffect(() => {
    let current = true;
    if (!imagePath || !isRaw) {
      setProfiles([]);
      return;
    }
    invoke<CameraProfileEntry[]>(Invokes.ListCameraProfiles, { imagePath })
      .then((list) => current && setProfiles(Array.isArray(list) ? list : []))
      .catch(() => current && setProfiles([]));
    return () => {
      current = false;
    };
  }, [imagePath, isRaw]);

  const lookName = (entry: LutEntry, language?: string) =>
    entry.isBuiltIn && LOOK_NAMES[entry.name]
      ? t(`studio.lut.names.${LOOK_NAMES[entry.name] as "warm"}`, { lng: language })
      : entry.name;

  const tiles = useMemo<ProfileTile[]>(() => {
    const list: ProfileTile[] = [];
    if (isRaw) {
      const camera = t("studio.profileBrowser.groups.camera");
      list.push({ key: "camera:", kind: "camera", group: camera, name: "Rawtonic", variant: { cameraProfile: null }, selected: !cameraProfile });
      if (cameraProfile && !profiles.some((p) => p.value === cameraProfile))
        list.push({
          key: `camera:${cameraProfile}`,
          kind: "camera",
          group: camera,
          name: cameraProfile.split(/[\\/]/).pop() ?? cameraProfile,
          variant: { cameraProfile },
          selected: true,
        });
      for (const p of profiles)
        list.push({ key: `camera:${p.value}`, kind: "camera", group: camera, name: p.name, variant: { cameraProfile: p.value }, selected: p.value === cameraProfile });
    }
    const looks = t("studio.profileBrowser.groups.looks");
    list.push({
      key: "look:",
      kind: "look",
      group: looks,
      name: t("studio.lut.none"),
      variant: { lutPath: null, lutIntensity: 100, lutIsSceneReferred: false },
      selected: !lutPath,
    });
    const look = (entry: LutEntry, group: string): ProfileTile => ({
      // A built-in's path is the app's install folder: its name stays.
      key: entry.isBuiltIn ? `look:builtin:${entry.name}` : `look:${entry.path}`,
      kind: "look",
      group,
      name: lookName(entry),
      variant: { lutPath: entry.path, lutIntensity: 100, lutIsSceneReferred: entry.isBuiltIn },
      selected: entry.path === lutPath,
      lut: entry,
    });
    for (const entry of luts) if (entry.isBuiltIn) list.push(look(entry, looks));
    const bw = t("studio.profileBrowser.groups.bw");
    list.push({ key: "bw:", kind: "bw", group: bw, name: t("studio.profileBrowser.colour"), variant: { blackAndWhite: false }, selected: !blackAndWhite });
    if (showBuiltins)
      for (const preset of builtins.filter((p) => p.presetType === "tool" && p.id.startsWith("bw-mix-")))
        list.push({
          key: `bw:${preset.id}`,
          kind: "bw",
          group: bw,
          name: presetName(t, preset),
          variant: { blackAndWhite: true, grayMixer: (preset.adjustments as Adjustments).grayMixer },
          selected: blackAndWhite && sameMixer(grayMixer, (preset.adjustments as Adjustments).grayMixer),
        });
    // Imported looks under their pack's (folder's) name, single files first.
    const imported = luts.filter((e) => !e.isBuiltIn);
    const single = imported.filter((e) => !e.group);
    for (const entry of single) list.push(look(entry, t("studio.profileBrowser.groups.imported")));
    const packs = [...new Set(imported.flatMap((e) => (e.group ? [e.group] : [])))].sort((a, b) =>
      a.localeCompare(b, undefined, { sensitivity: "base" }),
    );
    for (const pack of packs) for (const entry of imported.filter((e) => e.group === pack)) list.push(look(entry, pack));
    if (lutPath && !luts.some((e) => e.path === lutPath))
      list.push(look({ name: lutPath.split(/[\\/]/).pop() ?? lutPath, path: lutPath, isBuiltIn: false }, t("studio.profileBrowser.groups.imported")));
    return list;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isRaw, cameraProfile, profiles, lutPath, luts, blackAndWhite, grayMixer, builtins, showBuiltins, t]);

  const favouriteSet = useMemo(() => new Set(favourites ?? []), [favourites]);
  const shown = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    const matching = needle ? tiles.filter((tile) => tile.name.toLocaleLowerCase().includes(needle)) : tiles;
    const favouriteGroup = t("studio.profileBrowser.groups.favourites");
    const favs = matching.filter((tile) => favouriteSet.has(tile.key)).map((tile) => ({ ...tile, group: favouriteGroup }));
    const groups = new Map<string, ProfileTile[]>();
    for (const tile of [...favs, ...matching]) groups.set(tile.group, [...(groups.get(tile.group) ?? []), tile]);
    return [...groups];
  }, [tiles, query, favouriteSet, t]);

  const selectedCamera = profiles.find((p) => p.value === cameraProfile);
  const current = [
    isRaw ? (tiles.find((tile) => tile.kind === "camera" && tile.selected)?.name ?? "Rawtonic") : null,
    lutPath ? tiles.find((tile) => tile.kind === "look" && tile.selected)?.name : null,
  ]
    .filter(Boolean)
    .join(" · ");

  // Hover/focus preview: the renderer shows the override, nothing applied.
  const previewing = useRef(false);
  const endPreview = () => {
    if (!previewing.current) return;
    previewing.current = false;
    useEditorStore.getState().setEditor({ previewOverride: null });
  };
  const preview = (tile: ProfileTile) => {
    const { selectedImage, adjustments, setEditor } = useEditorStore.getState();
    if (!selectedImage?.isReady || disabled) return;
    previewing.current = true;
    timeHoverFrame();
    setEditor({ previewOverride: withVariant(adjustments, tileKeys(tile, adjustments)) });
  };
  useEffect(() => endPreview, []);
  useEffect(() => {
    if (!opened) endPreview();
  }, [opened]);

  const apply = (tile: ProfileTile) => {
    endPreview();
    const selectedImage = useEditorStore.getState().selectedImage;
    if (!selectedImage?.isReady || disabled) return;
    if (tile.kind === "camera") {
      const value = tile.variant.cameraProfile as string | null;
      setAdjustments((prev) => ({ ...prev, cameraProfile: value || undefined }));
    } else if (tile.kind === "look") {
      if (tile.lut) void handleLutSelect(tile.lut.path, tile.lut.isBuiltIn, tile.lut.isBuiltIn ? lookName(tile.lut, "en") : undefined);
      else
        setAdjustments((prev) => ({
          ...prev,
          lutPath: null,
          lutName: null,
          lutData: null,
          lutSize: 0,
          lutIntensity: 100,
          lutIsSceneReferred: false,
        }));
    } else {
      // A mix sets only these two keys: one step on the open photo.
      const mixer = tile.variant.grayMixer as Adjustments["grayMixer"] | undefined;
      setAdjustments((prev) => ({
        ...prev,
        blackAndWhite: Boolean(tile.variant.blackAndWhite),
        ...(mixer ? { grayMixer: { ...mixer } } : {}),
      }));
    }
  };

  const toggleFavourite = (tile: ProfileTile) => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (!appSettings) return;
    const list = appSettings.favoriteProfiles ?? [];
    const next = list.includes(tile.key) ? list.filter((key) => key !== tile.key) : [...list, tile.key];
    void handleSettingsChange({ ...appSettings, favoriteProfiles: next });
  };

  const importProfile = async () => {
    if (!imagePath) return;
    try {
      const source = await open({ multiple: false, filters: [{ name: t("studio.profile.dcpFilter"), extensions: ["dcp", "DCP"] }] });
      if (typeof source !== "string") return;
      const entry = await invoke<CameraProfileEntry>(Invokes.ImportCameraProfile, { source, imagePath });
      setError(null);
      setProfiles((list) => (list.some((p) => p.value === entry.value) ? list : [...list, entry]));
      setAdjustments((prev) => ({ ...prev, cameraProfile: entry.value }));
    } catch (e) {
      setError(String(e));
    }
  };

  const urls = useTileThumbnails(opened, tiles, imagePath);
  const grid = useRef<HTMLDivElement>(null);

  // Arrow keys move between tiles (and preview them; Down from the search
  // box enters them), Enter applies, Esc restores the photo and then
  // closes the browser.
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const buttons = [...(grid.current?.querySelectorAll<HTMLButtonElement>("button.studio-profile-tile") ?? [])];
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      if (previewing.current) endPreview();
      else setOpened(false);
      return;
    }
    if (at < 0) {
      // From the search box down into the tiles.
      if (e.key === "ArrowDown" && buttons.length > 0 && (e.target as HTMLElement).tagName === "INPUT") {
        e.preventDefault();
        e.stopPropagation();
        buttons[0].focus();
      }
      return;
    }
    let next = at;
    const box = buttons[at].getBoundingClientRect();
    if (e.key === "ArrowRight") next = Math.min(buttons.length - 1, at + 1);
    else if (e.key === "ArrowLeft") next = Math.max(0, at - 1);
    else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      const down = e.key === "ArrowDown";
      const candidates = buttons
        .map((button, index) => ({ index, r: button.getBoundingClientRect() }))
        .filter(({ r }) => (down ? r.top > box.top + 1 : r.top < box.top - 1));
      const rowTop = down ? Math.min(...candidates.map(({ r }) => r.top)) : Math.max(...candidates.map(({ r }) => r.top));
      const row = candidates.filter(({ r }) => Math.abs(r.top - rowTop) < 1);
      const nearest = row.sort((a, b) => Math.abs(a.r.left - box.left) - Math.abs(b.r.left - box.left))[0];
      if (nearest) next = nearest.index;
    } else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = buttons.length - 1;
    else return;
    // The browser's keys, not the photo navigation's.
    e.preventDefault();
    e.stopPropagation();
    buttons[next].focus();
  };

  return (
    <>
      <div className="studio-inline">
        <span>{t("studio.profile.label")}</span>
        <button
          type="button"
          className="studio-profile-current"
          aria-expanded={opened}
          aria-label={t("studio.profileBrowser.open")}
          title={t("studio.profileBrowser.openTitle")}
          disabled={disabled}
          onClick={() => setOpened((value) => !value)}
        >
          <span>{current || t("studio.lut.none")}</span>
          <LayoutGrid size={12} />
        </button>
      </div>
      {selectedCamera?.camera && (
        <div className="studio-hint">{t("studio.profile.forCamera", { camera: selectedCamera.camera })}</div>
      )}
      {lutPath && (
        <AdjustmentControl
          label={t("studio.lut.amount")}
          ariaLabel={t("studio.profileBrowser.amount")}
          min={0}
          max={200}
          value={intensity}
          defaultValue={100}
          disabled={disabled}
          onInteraction={onInteraction}
          onChange={(lutIntensity) => setAdjustments((prev) => ({ ...prev, lutIntensity }))}
        />
      )}
      {opened && (
        <div
          className={`studio-profile-browser studio-profile-${layout}`}
          role="region"
          aria-label={t("studio.profileBrowser.title")}
          onKeyDown={onKeyDown}
        >
          <div className="studio-profile-bar">
            <input
              // Opening the browser starts in its search: typing filters,
              // Down enters the tiles.
              autoFocus
              type="search"
              aria-label={t("studio.profileBrowser.search")}
              placeholder={t("studio.profileBrowser.search")}
              value={query}
              onChange={(e) => {
                // Filtering may remove the previewed tile.
                endPreview();
                setQuery(e.target.value);
              }}
            />
            <button
              type="button"
              aria-pressed={layout === "grid"}
              aria-label={t("studio.profileBrowser.grid")}
              title={t("studio.profileBrowser.grid")}
              onClick={() => setLayout("grid")}
            >
              <LayoutGrid size={12} />
            </button>
            <button
              type="button"
              aria-pressed={layout === "list"}
              aria-label={t("studio.profileBrowser.list")}
              title={t("studio.profileBrowser.list")}
              onClick={() => setLayout("list")}
            >
              <List size={12} />
            </button>
          </div>
          <div
            ref={grid}
            className="studio-profile-groups"
            onMouseLeave={endPreview}
            // Focus leaving the tiles ends their preview, as the pointer does.
            onBlur={(e) => {
              if (!(e.relatedTarget instanceof HTMLElement && e.relatedTarget.classList.contains("studio-profile-tile")))
                endPreview();
            }}
          >
            {shown.length === 0 && <div className="studio-hint">{t("studio.profileBrowser.noMatch")}</div>}
            {shown.map(([group, list]) => (
              <section key={group} aria-label={group}>
                <h4>{group}</h4>
                <div className="studio-profile-tiles">
                  {list.map((tile) => {
                    const favourite = favouriteSet.has(tile.key);
                    return (
                      <div key={tile.key} className="studio-profile-cell" data-tile={tile.key}>
                        <button
                          type="button"
                          className="studio-profile-tile"
                          aria-pressed={tile.selected}
                          title={tile.name}
                          disabled={disabled}
                          onMouseEnter={() => preview(tile)}
                          onFocus={() => preview(tile)}
                          onClick={() => apply(tile)}
                        >
                          <span className="studio-profile-thumb">
                            {urls.get(tile.key) ? <img src={urls.get(tile.key)} alt="" draggable={false} /> : null}
                          </span>
                          <span className="studio-profile-name">{tile.name}</span>
                        </button>
                        <button
                          type="button"
                          className="studio-profile-star"
                          aria-pressed={favourite}
                          aria-label={t(favourite ? "studio.profileBrowser.unfavourite" : "studio.profileBrowser.favourite", { name: tile.name })}
                          title={t(favourite ? "studio.profileBrowser.unfavourite" : "studio.profileBrowser.favourite", { name: tile.name })}
                          tabIndex={-1}
                          onClick={() => toggleFavourite(tile)}
                        >
                          <Star size={12} fill={favourite ? "currentColor" : "none"} />
                        </button>
                      </div>
                    );
                  })}
                </div>
              </section>
            ))}
          </div>
          {isRaw && (
            <div className="studio-inline">
              <button type="button" disabled={disabled} onClick={() => void importProfile()}>
                {t("studio.profile.importDcp")}
              </button>
            </div>
          )}
          {error && (
            <div className="studio-hint" role="alert">
              <OperationError detail={error} />
            </div>
          )}
        </div>
      )}
    </>
  );
}

/** Times hover → the next finished frame (QA timing). */
function timeHoverFrame() {
  const started = performance.now();
  const frames = useEditorStore.getState().previewFrames;
  const stop = useEditorStore.subscribe((state) => {
    if (state.previewFrames === frames) return;
    stop();
    if (state.previewOverride) recordTiming("hoverFrame", performance.now() - started);
  });
  setTimeout(stop, 5000);
}

/** A timing record (`[PERF]` in the app log of profiling builds). */
function recordTiming(stage: string, ms: number) {
  reportPerformance(`profile_browser_${stage}`, { ms: Math.round(ms * 10) / 10 });
}

/**
 * Object URLs of the tiles on screen, rendered natively from the shown
 * preview base (`profile_thumbnails`) in batches; again after an edit
 * (the old tiles stay until the new ones arrive), never during a drag.
 * Closing cancels what is still rendering and frees the tiles.
 */
function useTileThumbnails(opened: boolean, tiles: ProfileTile[], imagePath: string | undefined) {
  const [, redraw] = useReducer((n: number) => n + 1, 0);
  const urls = useRef(new Map<string, string>());
  const renderedFor = useRef(new Map<string, unknown>());
  const tilesRef = useRef(tiles);
  tilesRef.current = tiles;
  // No preview base of the recipe yet: ask again with the next frame.
  const waiting = useRef(false);
  const [visible, setVisible] = useState<Set<string>>(new Set());
  const openedAt = useRef(0);
  // Bumped by edits (not during drags) and, while waiting, by frames: the
  // browser re-renders for them only while it is open.
  const [tick, bump] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    if (!opened) return;
    openedAt.current = performance.now();
    let last = useEditorStore.getState();
    return useEditorStore.subscribe((state) => {
      const before = last;
      last = state;
      if (state.isSliderDragging) return;
      if (
        state.adjustments !== before.adjustments ||
        before.isSliderDragging ||
        (waiting.current && state.previewFrames !== before.previewFrames)
      )
        bump();
    });
  }, [opened]);

  // Which tiles are on screen.
  useEffect(() => {
    if (!opened) return;
    const seen = new Set<string>();
    let observer: IntersectionObserver | null = null;
    let raf = 0;
    const watch = () => {
      const root = document.querySelector(".studio-profile-browser");
      if (!root || typeof IntersectionObserver === "undefined") {
        // No observer (tests): every tile counts as shown.
        setVisible(new Set(tilesRef.current.map((tile) => tile.key)));
        return;
      }
      observer = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          const key = (entry.target as HTMLElement).dataset.tile;
          if (!key) continue;
          if (entry.isIntersecting) seen.add(key);
          else seen.delete(key);
        }
        setVisible(new Set(seen));
      });
      root.querySelectorAll<HTMLElement>("[data-tile]").forEach((cell) => observer?.observe(cell));
    };
    raf = requestAnimationFrame(watch);
    return () => {
      cancelAnimationFrame(raf);
      observer?.disconnect();
    };
  }, [opened, tiles]);

  useEffect(() => {
    if (!opened || !imagePath) return;
    let gone = false;
    let generation = 0;
    let retry: ReturnType<typeof setTimeout> | null = null;
    const run = async () => {
      const state = useEditorStore.getState();
      if (gone || state.isSliderDragging || !state.selectedImage?.isReady || state.selectedImage.path !== imagePath) return;
      const recipe = state.adjustments;
      const want = tilesRef.current.filter((tile) => visible.has(tile.key) && renderedFor.current.get(tile.key) !== recipe);
      if (want.length === 0) return;
      const size = Math.round(TILE_POINTS * Math.min(2, window.devicePixelRatio || 1));
      for (let at = 0; at < want.length && !gone; at += BATCH) {
        const batch = want.slice(at, at + BATCH);
        generation = ++generations;
        let answer: ArrayBuffer;
        try {
          answer = await invoke<ArrayBuffer>(Invokes.ProfileThumbnails, {
            path: imagePath,
            adjustments: recipe,
            variants: batch.map((tile) => tileKeys(tile, recipe)),
            size,
            generation,
          });
        } catch (error) {
          console.error("Profile thumbnails failed:", error);
          return;
        }
        if (gone) return;
        if (!answer || answer.byteLength === 0) {
          waiting.current = true;
          return;
        }
        waiting.current = false;
        splitTiles(answer, batch.length).forEach((blob, index) => {
          if (!blob) return;
          const key = batch[index].key;
          const old = urls.current.get(key);
          if (old) URL.revokeObjectURL(old);
          urls.current.set(key, URL.createObjectURL(blob));
          renderedFor.current.set(key, recipe);
        });
        redraw();
      }
      if (!gone && openedAt.current && visible.size > 0 && [...visible].every((key) => urls.current.has(key))) {
        recordTiming("openToVisibleTiles", performance.now() - openedAt.current);
        openedAt.current = 0;
      }
    };
    // Edits come in bursts (a click, a pasted recipe): render after them.
    retry = setTimeout(() => void run(), 60);
    return () => {
      gone = true;
      if (retry) clearTimeout(retry);
      if (generation) void invoke(Invokes.CancelProfileThumbnails, { generation }).catch(() => undefined);
    };
  }, [opened, imagePath, visible, tick]);

  // Closed, another photograph or unmounted: free every tile.
  useEffect(() => {
    const map = urls.current;
    const rendered = renderedFor.current;
    return () => {
      map.forEach((url) => URL.revokeObjectURL(url));
      map.clear();
      rendered.clear();
    };
  }, [opened, imagePath]);
  return urls.current;
}
