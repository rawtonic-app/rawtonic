import { useMemo, useRef, useState, type ComponentProps, type PointerEvent as ReactPointerEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { useShallow } from 'zustand/react/shallow';
import { Flag, GripVertical, PanelBottom, PanelLeft, PanelRight, Star, Tag, X } from 'lucide-react';
import StudioBrowser from './StudioBrowser';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useUIStore } from '../../store/useUIStore';
import { COLOR_LABEL_NAMES } from '../../entities/photo/colorLabel';
import { DOCK_WIDTH, dockSideAt, readFilmstripDock, type DockSide } from '../../utils/filmstripDock';
import { steppedWidth } from '../../utils/sidePanelWidths';

type BrowserProps = ComponentProps<typeof StudioBrowser>;

/** Pointer travel (px) from which a press on the header is a drag. */
const DRAG_SLOP = 6;

/**
 * Develop's filmstrip docked as a vertical grid in a column beside the
 * photograph (Capture One's browser can sit in a side panel; Lightroom's
 * filmstrip is bottom-only). It is the same browser as the strip with the
 * same photographs, thumbnails, selection and arrow keys; its header has the
 * count, a quick filter (rating, flag, colour label — the library's own
 * filter, so the arrows step through what it shows), the file-name toggle and
 * the place of the strip. Ctrl/⌘ + wheel sets the thumbnail size; dragging
 * the header to the left or right edge of the window, or the bottom, moves it.
 */
export default function StudioFilmstripDock({
  side,
  shownWidth,
  maxWidth,
  ...browser
}: {
  side: Exclude<DockSide, 'bottom'>;
  /** The width it is drawn at and the widest it may take now
   * (sidePanelWidths: the photograph keeps its room). */
  shownWidth?: number;
  maxWidth?: number;
} & BrowserProps) {
  const { t } = useTranslation();
  const stored = useUIStore((s) => s.uiVisibility.filmstripDock);
  const dock = useMemo(() => readFilmstripDock(stored), [stored]);
  const setDock = useUIStore((s) => s.setFilmstripDock);
  const { filter, setFilter } = useLibraryStore(
    useShallow((s) => ({ filter: s.filterCriteria, setFilter: s.setFilterCriteria }))
  );
  const flag = filter.flag ?? 'all';
  const filtering = flag !== 'all' || filter.rating !== 0 || filter.colors.length > 0;
  const [drop, setDrop] = useState<{ side: DockSide; area: DOMRect } | null>(null);
  const press = useRef<{ x: number; y: number; dragging: boolean } | null>(null);
  const aside = useRef<HTMLElement>(null);

  const dropAt = (e: ReactPointerEvent) => {
    const area = aside.current?.closest('.studio-body')?.getBoundingClientRect();
    const target = area ? dockSideAt(e.clientX, e.clientY, area) : null;
    return area && target ? { side: target, area } : null;
  };
  const onHeaderDown = (e: ReactPointerEvent<HTMLElement>) => {
    if (e.button !== 0 || (e.target as Element).closest('button')) return;
    press.current = { x: e.clientX, y: e.clientY, dragging: false };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onHeaderMove = (e: ReactPointerEvent<HTMLElement>) => {
    const p = press.current;
    if (!p) return;
    if (!p.dragging && Math.hypot(e.clientX - p.x, e.clientY - p.y) < DRAG_SLOP) return;
    p.dragging = true;
    setDrop(dropAt(e));
  };
  const endDrag = (e: ReactPointerEvent<HTMLElement>, cancelled: boolean) => {
    const p = press.current;
    press.current = null;
    setDrop(null);
    if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
    if (!p?.dragging || cancelled) return;
    const target = dropAt(e);
    if (target && target.side !== side) setDock({ side: target.side });
  };

  const places: { value: DockSide; Icon: typeof PanelLeft; label: string }[] = [
    { value: 'left', Icon: PanelLeft, label: t('studio.workspace.dock.left') },
    { value: 'bottom', Icon: PanelBottom, label: t('studio.workspace.dock.bottom') },
    { value: 'right', Icon: PanelRight, label: t('studio.workspace.dock.right') }
  ];
  const colorNames = {
    red: t('studio.library.colorLabels.red'),
    yellow: t('studio.library.colorLabels.yellow'),
    green: t('studio.library.colorLabels.green'),
    blue: t('studio.library.colorLabels.blue'),
    purple: t('studio.library.colorLabels.purple')
  };
  const colorName = (name: keyof typeof colorNames) => colorNames[name];

  const shown = shownWidth ?? dock.width;
  const cap = maxWidth ?? DOCK_WIDTH.max;
  // A step starts from the drawn width; one that cannot move it keeps the
  // stored width for a wider window.
  const resize = (delta: number) => {
    const next = steppedWidth(dock.width, shown, delta, DOCK_WIDTH.min, cap);
    if (next !== dock.width) setDock({ width: next });
  };
  return (
    <aside
      ref={aside}
      className={`studio-dock is-${side}`}
      style={{ width: shown }}
      aria-label={t('studio.workspace.dock.title')}
    >
      <header
        className="studio-dock-header"
        title={t('studio.workspace.dock.dragHint')}
        onPointerDown={onHeaderDown}
        onPointerMove={onHeaderMove}
        onPointerUp={(e) => endDrag(e, false)}
        onPointerCancel={(e) => endDrag(e, true)}
      >
        <GripVertical size={12} aria-hidden />
        <strong>{t('studio.workspace.dock.title')}</strong>
        <span className="studio-dock-count">
          {t('studio.workspace.footer.photographs', { count: browser.files.length, n: browser.files.length })}
        </span>
        <span className="studio-spacer" />
        {places.map(({ value, Icon, label }) => (
          <button
            key={value}
            type="button"
            aria-pressed={value === side}
            aria-label={label}
            title={label}
            onClick={() => setDock({ side: value })}
          >
            <Icon size={13} />
          </button>
        ))}
      </header>
      <div className="studio-dock-filter" role="group" aria-label={t('studio.workspace.dock.quickFilter')}>
        <span className="studio-dock-stars" role="group" aria-label={t('studio.workspace.dock.minRating')}>
          {[1, 2, 3, 4, 5].map((n) => (
            <button
              key={n}
              type="button"
              aria-pressed={filter.rating === n}
              aria-label={t('studio.workspace.dock.atLeastStars', { n })}
              title={t('studio.workspace.dock.atLeastStars', { n })}
              onClick={() => setFilter({ rating: filter.rating === n ? 0 : n })}
            >
              <Star size={11} fill={filter.rating >= n ? 'currentColor' : 'none'} />
            </button>
          ))}
        </span>
        <button
          type="button"
          aria-pressed={flag === 'picked'}
          aria-label={t('studio.library.flags.picked')}
          title={t('studio.library.flags.picked')}
          onClick={() => setFilter({ flag: flag === 'picked' ? 'all' : 'picked' })}
        >
          <Flag size={11} />
        </button>
        <button
          type="button"
          aria-pressed={flag === 'rejected'}
          aria-label={t('studio.library.flags.rejected')}
          title={t('studio.library.flags.rejected')}
          onClick={() => setFilter({ flag: flag === 'rejected' ? 'all' : 'rejected' })}
        >
          <X size={12} />
        </button>
        {COLOR_LABEL_NAMES.map((name) => (
          <button
            key={name}
            type="button"
            className={`studio-dock-color is-${name}`}
            aria-pressed={filter.colors.includes(name)}
            aria-label={colorName(name)}
            title={colorName(name)}
            onClick={() =>
              setFilter((prev) => ({
                ...prev,
                colors: prev.colors.includes(name) ? prev.colors.filter((c) => c !== name) : [...prev.colors, name]
              }))
            }
          />
        ))}
        {filtering && (
          <button
            type="button"
            className="studio-dock-clear"
            aria-label={t('studio.library.filters.clearFilters')}
            title={t('studio.library.filters.clearFilters')}
            onClick={() => setFilter({ flag: 'all', rating: 0, colors: [] })}
          >
            <X size={12} />
          </button>
        )}
        <span className="studio-spacer" />
        <button
          type="button"
          aria-pressed={dock.names}
          aria-label={t('studio.workspace.dock.fileNames')}
          title={t('studio.workspace.dock.fileNames')}
          onClick={() => setDock({ names: !dock.names })}
        >
          <Tag size={12} />
        </button>
      </div>
      <div className="studio-dock-body" title={t('studio.workspace.dock.sizeHint')}>
        <StudioBrowser
          {...browser}
          film
          dock={{ size: dock.size, names: dock.names, onSize: (size) => setDock({ size }) }}
        />
      </div>
      <div
        className="studio-dock-resizer"
        role="separator"
        tabIndex={0}
        aria-label={t('studio.workspace.dock.width')}
        aria-orientation="vertical"
        aria-valuemin={DOCK_WIDTH.min}
        aria-valuemax={cap}
        aria-valuenow={shown}
        onKeyDown={(e) => {
          if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
          e.preventDefault();
          resize((e.key === 'ArrowRight' ? 10 : -10) * (side === 'right' ? -1 : 1));
        }}
        onPointerDown={(e) => {
          e.preventDefault();
          e.currentTarget.setPointerCapture(e.pointerId);
        }}
        onPointerMove={(e) => {
          if (e.currentTarget.hasPointerCapture(e.pointerId)) resize(e.movementX * (side === 'right' ? -1 : 1));
        }}
        onPointerUp={(e) => e.currentTarget.releasePointerCapture(e.pointerId)}
      />
      {drop && (
        // The place it would take, shown while dragging (the zone is a
        // quarter of the window's width, the lower fifth for the strip).
        <div
          className={`studio-dock-drop is-${drop.side}${drop.side === side ? ' is-here' : ''}`}
          style={{
            left: drop.side === 'right' ? drop.area.left + drop.area.width * 0.75 : drop.area.left,
            top: drop.side === 'bottom' ? drop.area.top + drop.area.height * 0.8 : drop.area.top,
            width: drop.side === 'bottom' ? drop.area.width : drop.area.width * 0.25,
            height: drop.side === 'bottom' ? drop.area.height * 0.2 : drop.area.height
          }}
          aria-hidden
        >
          {t(`studio.workspace.dock.${drop.side}`)}
        </div>
      )}
    </aside>
  );
}
