// The second window's page (second_window.rs): the photograph is drawn
// natively under this transparent webview, or on Linux by the page from
// JPEG frames (SecondViewFrames.tsx); the page sends the zoom and pan and
// shows a toolbar that hides while the pointer rests.
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useTranslation } from 'react-i18next';
import { Lock, LockOpen, Maximize, Minimize } from 'lucide-react';
import clsx from 'clsx';
import { Invokes } from '../ui/AppProperties';
import {
  SECOND_WINDOW_EVENT,
  SECOND_WINDOW_FRAMES_EVENT,
  closedSecondWindow,
  type SecondWindowFrames,
  type SecondWindowState
} from '../../store/useSecondWindowStore';
import { applyInterfaceShade } from '../../utils/themes';
import { panBy, toggleAt, type Zoom } from './secondViewZoom';
import SecondViewFrames from './SecondViewFrames';

/** The toolbar hides this long after the pointer last moved. */
const TOOLBAR_IDLE_MS = 2000;
/** A press that moves less than this (CSS px) is a click, not a pan. */
const CLICK_SLOP = 4;

function fileName(path: string | null): string {
  if (!path) return '';
  const base = path.split(/[\\/]/).pop() ?? path;
  return base.replace(/\?vc=.*$/, '');
}

function deviceArea(): [number, number] {
  const dpr = window.devicePixelRatio || 1;
  return [window.innerWidth * dpr, window.innerHeight * dpr];
}

function readLayout(): { area: [number, number]; dpr: number } {
  return { area: deviceArea(), dpr: window.devicePixelRatio || 1 };
}

export default function SecondView() {
  const { t } = useTranslation();
  const [state, setState] = useState<SecondWindowState>(closedSecondWindow);
  const [toolbar, setToolbar] = useState(true);
  const [fullScreen, setFullScreen] = useState(false);
  const zoomRef = useRef<Zoom | null>(null);
  const press = useRef<{ x: number; y: number; moved: boolean } | null>(null);
  const idleTimer = useRef<number | undefined>(undefined);
  const overToolbar = useRef(false);
  const pending = useRef<number | null>(null);
  // Where the page draws the photograph: its frames, the zoom it shows at
  // (set at once while panning, not after the round trip) and its size.
  const [frames, setFrames] = useState<SecondWindowFrames | null>(null);
  const [viewZoom, setViewZoom] = useState<Zoom | null>(null);
  const [layout, setLayout] = useState(readLayout);

  useEffect(() => {
    zoomRef.current = state.zoom;
    // Not while panning: the zoom echoed back would pull the view behind.
    if (!press.current?.moved && pending.current === null) setViewZoom(state.zoom);
  }, [state.zoom]);

  const pageDraws = state.pageDraws;
  useEffect(() => {
    if (!pageDraws) return;
    void invoke<SecondWindowFrames | null>(Invokes.SecondWindowFrames)
      .then((current) => current && setFrames(current))
      .catch(() => {});
    const unlisten = listen<SecondWindowFrames>(SECOND_WINDOW_FRAMES_EVENT, (event) => setFrames(event.payload));
    // Laid out again when the window's size or its display's scale
    // changes (moved to a display of another scale: no resize event).
    let frame = 0;
    const refresh = () => {
      setLayout(readLayout());
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => setLayout(readLayout()));
    };
    let media: MediaQueryList | null = null;
    const onScale = () => {
      refresh();
      watchScale();
    };
    const watchScale = () => {
      media?.removeEventListener('change', onScale);
      media = window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
      media.addEventListener('change', onScale);
    };
    watchScale();
    window.addEventListener('resize', refresh);
    const win = getCurrentWindow();
    const stops = [win.onResized(refresh), win.onScaleChanged(refresh)];
    return () => {
      void unlisten.then((stop) => stop());
      window.removeEventListener('resize', refresh);
      media?.removeEventListener('change', onScale);
      cancelAnimationFrame(frame);
      for (const stop of stops) void stop.then((s) => s());
    };
  }, [pageDraws]);

  useEffect(() => {
    void invoke<{ language?: string; interfaceShade?: string }>(Invokes.LoadSettings)
      .then((settings) => {
        applyInterfaceShade(settings?.interfaceShade);
        if (settings?.language)
          void import('i18next').then(({ default: i18n }) => i18n.changeLanguage(settings.language));
      })
      .catch(() => {});
    void invoke<SecondWindowState>(Invokes.SecondWindowState)
      .then(setState)
      .catch(() => {});
    const unlisten = listen<SecondWindowState>(SECOND_WINDOW_EVENT, (event) => setState(event.payload));
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, []);

  useEffect(() => {
    const name = fileName(state.path);
    document.title = name ? t('secondView.windowTitle', { name }) : t('secondView.windowTitleEmpty');
    void getCurrentWindow()
      .setTitle(document.title)
      .catch(() => {});
  }, [state.path, t]);

  useEffect(() => {
    const win = getCurrentWindow();
    const update = () =>
      void win
        .isFullscreen()
        .then(setFullScreen)
        .catch(() => {});
    update();
    const unlisten = win.onResized(update);
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, []);

  const sendZoom = useCallback((zoom: Zoom | null) => {
    zoomRef.current = zoom;
    setViewZoom(zoom);
    // At most one zoom per frame while panning.
    if (pending.current !== null) return;
    pending.current = requestAnimationFrame(() => {
      pending.current = null;
      void invoke(Invokes.SecondWindowSetZoom, { zoom: zoomRef.current }).catch(() => {});
    });
  }, []);

  const wake = useCallback(() => {
    setToolbar(true);
    window.clearTimeout(idleTimer.current);
    idleTimer.current = window.setTimeout(() => {
      if (!overToolbar.current) setToolbar(false);
    }, TOOLBAR_IDLE_MS);
  }, []);

  useEffect(() => {
    wake();
    return () => window.clearTimeout(idleTimer.current);
  }, [wake]);

  const toggleZoom = useCallback(
    (point?: [number, number]) => {
      if (!state.shown) return;
      const area = deviceArea();
      sendZoom(toggleAt(area, state.shown, zoomRef.current, point ?? [area[0] / 2, area[1] / 2]));
    },
    [sendZoom, state.shown]
  );

  const setLocked = useCallback((locked: boolean) => {
    void invoke(Invokes.SecondWindowSetLocked, { locked }).catch(() => {});
  }, []);

  const toggleFullScreen = useCallback(() => {
    void invoke(Invokes.SetWindowFullScreen, { on: !fullScreen }).catch(() => {});
  }, [fullScreen]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.code === 'KeyF' && e.shiftKey) {
        e.preventDefault();
        void invoke(Invokes.SecondWindowSetOpen, { open: false }).catch(() => {});
        return;
      }
      if (e.shiftKey) return;
      switch (e.code) {
        case 'Space':
        case 'KeyZ':
          e.preventDefault();
          toggleZoom();
          break;
        case 'KeyL':
          e.preventDefault();
          setLocked(!state.locked);
          break;
        case 'KeyF':
          e.preventDefault();
          toggleFullScreen();
          break;
        case 'Escape':
          if (fullScreen) {
            e.preventDefault();
            void invoke(Invokes.SetWindowFullScreen, { on: false }).catch(() => {});
          }
          break;
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [fullScreen, setLocked, state.locked, toggleFullScreen, toggleZoom]);

  const dpr = () => window.devicePixelRatio || 1;

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    press.current = { x: e.clientX, y: e.clientY, moved: false };
  };

  const onPointerMove = (e: React.PointerEvent) => {
    wake();
    const start = press.current;
    if (!start) return;
    const dx = e.clientX - start.x;
    const dy = e.clientY - start.y;
    if (!start.moved && Math.hypot(dx, dy) < CLICK_SLOP) return;
    start.moved = true;
    const zoom = zoomRef.current;
    if (zoom && state.shown) {
      sendZoom(panBy(deviceArea(), state.shown, zoom, [dx * dpr(), dy * dpr()]));
    }
    press.current = { x: e.clientX, y: e.clientY, moved: true };
  };

  const onPointerUp = (e: React.PointerEvent) => {
    const start = press.current;
    press.current = null;
    if (start && !start.moved) toggleZoom([e.clientX * dpr(), e.clientY * dpr()]);
  };

  const zoomed = !!state.zoom;
  const hasPhoto = !!state.path;
  const cursor = !hasPhoto ? 'default' : zoomed ? (press.current?.moved ? 'grabbing' : 'grab') : 'zoom-in';

  return (
    <div
      className={clsx(
        'relative h-full w-full select-none overflow-hidden',
        (!hasPhoto || pageDraws) && 'bg-bg-primary'
      )}
      style={{ cursor: toolbar || !fullScreen ? cursor : 'none' }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={() => (press.current = null)}
    >
      {pageDraws && hasPhoto && frames?.path === state.path && (
        <SecondViewFrames
          whole={frames.whole}
          region={frames.region}
          area={layout.area}
          zoom={viewZoom}
          dpr={layout.dpr}
        />
      )}
      {!hasPhoto && (
        <div className="flex h-full w-full items-center justify-center text-sm text-text-secondary">
          {t('secondView.empty')}
        </div>
      )}
      <div
        role="toolbar"
        aria-label={t('secondView.toolbar')}
        className={clsx(
          'absolute left-1/2 top-3 flex -translate-x-1/2 items-center gap-1 rounded-lg bg-bg-secondary/90 px-2 py-1 text-xs text-text-primary shadow-lg transition-opacity duration-200',
          toolbar ? 'opacity-100' : 'pointer-events-none opacity-0'
        )}
        onPointerDown={(e) => e.stopPropagation()}
        onPointerUp={(e) => e.stopPropagation()}
        onPointerEnter={() => (overToolbar.current = true)}
        onPointerLeave={() => (overToolbar.current = false)}
      >
        <span className="max-w-[24ch] truncate px-1 text-text-secondary" title={state.path ?? undefined}>
          {fileName(state.path)}
        </span>
        <button
          type="button"
          className={clsx('rounded px-2 py-0.5', !zoomed ? 'bg-accent text-button-text' : 'hover:bg-surface')}
          aria-pressed={!zoomed}
          disabled={!hasPhoto}
          onClick={() => zoomed && sendZoom(null)}
        >
          {t('secondView.fit')}
        </button>
        <button
          type="button"
          className={clsx('rounded px-2 py-0.5', zoomed ? 'bg-accent text-button-text' : 'hover:bg-surface')}
          aria-pressed={zoomed}
          disabled={!hasPhoto}
          onClick={() => !zoomed && toggleZoom()}
        >
          {t('secondView.actualSize')}
        </button>
        <button
          type="button"
          className={clsx(
            'flex items-center gap-1 rounded px-2 py-0.5',
            state.locked ? 'bg-accent text-button-text' : 'hover:bg-surface'
          )}
          aria-pressed={state.locked}
          title={state.locked ? t('secondView.unlockHint') : t('secondView.lockHint')}
          disabled={!hasPhoto}
          onClick={() => setLocked(!state.locked)}
        >
          {state.locked ? <Lock size={13} /> : <LockOpen size={13} />}
          {state.locked ? t('secondView.locked') : t('secondView.lock')}
        </button>
        <button
          type="button"
          className="rounded p-1 hover:bg-surface"
          aria-label={fullScreen ? t('secondView.exitFullScreen') : t('secondView.fullScreen')}
          title={fullScreen ? t('secondView.exitFullScreen') : t('secondView.fullScreen')}
          onClick={toggleFullScreen}
        >
          {fullScreen ? <Minimize size={13} /> : <Maximize size={13} />}
        </button>
      </div>
    </div>
  );
}
