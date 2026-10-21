import { useRef, type KeyboardEvent, type PointerEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronLeft, ChevronRight } from 'lucide-react';
import { useEditorStore } from '../../../store/useEditorStore';
import { useSnapshotStore } from '../../../store/useSnapshotStore';
import { beforeSnapshot } from '../../../utils/beforeSnapshot';
import { DEFAULT_SPLIT_POSITION, nudgeSplitPosition, splitPositionAt } from '../../../utils/beforeAfter';

/**
 * The before/after split's divider, handle and labels over the viewer. The
 * images are composed natively (display.wgsl) or clipped browser previews
 * on Linux; this draws only the controls. The divider is a place in the viewer (a share of its width),
 * so it stays put while the photograph is panned or zoomed and from one
 * photograph to the next, as in Capture One.
 */
export default function SplitViewOverlay() {
  const { t } = useTranslation();
  const splitView = useEditorStore((state) => state.splitView);
  const position = useEditorStore((state) => state.splitPosition);
  // While \ is held the whole photograph shows before: no divider, no labels.
  const peeking = useEditorStore((state) => state.showOriginal);
  // The snapshot compared with names the before side.
  useEditorStore((state) => state.beforeSnapshot);
  useSnapshotStore((state) => state.snapshots);
  const compared = beforeSnapshot();
  const rootRef = useRef<HTMLDivElement>(null);
  const draggingRef = useRef(false);

  if (!splitView || peeking) return null;

  const setPosition = (next: number) => useEditorStore.getState().setEditor({ splitPosition: next });
  const moveTo = (clientX: number) => {
    const rect = rootRef.current?.getBoundingClientRect();
    if (rect) setPosition(splitPositionAt(clientX, rect));
  };

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.focus();
    draggingRef.current = true;
    event.currentTarget.setPointerCapture(event.pointerId);
    moveTo(event.clientX);
  };
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    moveTo(event.clientX);
  };
  const onPointerUp = (event: PointerEvent<HTMLDivElement>) => {
    draggingRef.current = false;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const next = nudgeSplitPosition(position, event.key, event.shiftKey);
    if (next === null) return;
    event.preventDefault();
    event.stopPropagation();
    setPosition(next);
  };

  const percent = position * 100;
  const label = 'pointer-events-none absolute top-2 rounded px-2 py-0.5 text-xs font-medium text-white bg-black/55 select-none';

  return (
    <div ref={rootRef} className="absolute inset-0 pointer-events-none z-[60]" data-testid="split-view-overlay">
      <span className={`${label} left-2`}>
        {compared
          ? t('studio.workspace.viewer.splitBeforeSnapshot', { name: compared.name })
          : t('studio.workspace.viewer.splitBefore')}
      </span>
      <span className={`${label} right-2`}>{t('studio.workspace.viewer.splitAfter')}</span>
      <div
        role="slider"
        tabIndex={0}
        aria-label={t('studio.workspace.viewer.splitDivider')}
        aria-orientation="horizontal"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(percent)}
        className="group pointer-events-auto absolute top-0 bottom-0 w-3 -translate-x-1/2 cursor-ew-resize touch-none outline-none"
        style={{ left: `${percent}%` }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onDoubleClick={() => setPosition(DEFAULT_SPLIT_POSITION)}
        onKeyDown={onKeyDown}
      >
        <div className="absolute top-0 bottom-0 left-1/2 w-px -translate-x-1/2 bg-white/90 shadow-[0_0_2px_rgba(0,0,0,0.8)]" />
        <div className="absolute top-1/2 left-1/2 flex h-8 w-8 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-full border border-white/90 bg-black/60 text-white shadow-[0_1px_4px_rgba(0,0,0,0.6)] group-focus-visible:ring-2 group-focus-visible:ring-accent">
          <ChevronLeft size={12} strokeWidth={2.5} className="-mr-0.5" />
          <ChevronRight size={12} strokeWidth={2.5} className="-ml-0.5" />
        </div>
      </div>
    </div>
  );
}
