import { useEffect, useRef, useState, type RefObject } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Copy, Minus, Square, X } from 'lucide-react';
import { Invokes } from '../components/ui/AppProperties';

/** DOM bounds already include CSS interface zoom; only display DPI remains. */
export function physicalButtonRect(box: Pick<DOMRect, 'left' | 'top' | 'width' | 'height'>, scale: number) {
  return { x: box.left * scale, y: box.top * scale, width: box.width * scale, height: box.height * scale };
}

interface NativeButtonState {
  hovered: boolean;
  pressed: boolean;
}

/**
 * Windows 11 snap layouts: a native overlay over the maximize button answers
 * the system's hit test as a maximize button (src-tauri/src/snap_layout.rs).
 * This keeps its place in physical pixels in step with layout, interface
 * size and display scale, and returns the hover/pressed state the overlay
 * takes from the webview.
 */
function useSnapLayoutButton(button: RefObject<HTMLButtonElement | null>, enabled: boolean): NativeButtonState {
  const [state, setState] = useState<NativeButtonState>({ hovered: false, pressed: false });

  useEffect(() => {
    const element = button.current;
    if (!enabled || !element) return;
    let disposed = false;
    let sent = '';
    const report = () => {
      if (disposed) return;
      const box = element.getBoundingClientRect();
      const scale = window.devicePixelRatio || 1;
      const rect = physicalButtonRect(box, scale);
      const modalOpen = !!document.querySelector('dialog[open], [aria-modal="true"]');
      const key = JSON.stringify({ rect, modalOpen });
      if (key === sent) return;
      sent = key;
      void invoke(Invokes.SetMaximizeButtonRect, { rect, modalOpen }).catch(console.error);
    };
    report();
    const dialogs = new MutationObserver(report);
    dialogs.observe(document.body, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ['open', 'aria-modal']
    });
    // CSS zoom and compact-bar classes live on ancestors. Their content
    // boxes need not resize when the drawn button moves.
    const layout = new MutationObserver(report);
    for (let ancestor: HTMLElement | null = element; ancestor; ancestor = ancestor.parentElement) {
      layout.observe(ancestor, { attributes: true, attributeFilter: ['class', 'style'] });
    }
    const observer = new ResizeObserver(report);
    observer.observe(element);
    if (element.parentElement) observer.observe(element.parentElement);
    observer.observe(document.documentElement);
    // Also a display-scale change (another monitor), which resizes nothing in CSS pixels.
    window.addEventListener('resize', report);
    let unlisten: (() => void) | undefined;
    void listen<NativeButtonState>('maximize-button-state', (event) => setState(event.payload))
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch(console.error);
    return () => {
      disposed = true;
      observer.disconnect();
      dialogs.disconnect();
      layout.disconnect();
      window.removeEventListener('resize', report);
      unlisten?.();
      setState({ hovered: false, pressed: false });
      void invoke(Invokes.SetMaximizeButtonRect, { rect: null }).catch(console.error);
    };
  }, [button, enabled]);

  return state;
}

/** Windows and Linux window controls share the session bar instead of adding a second row. */
export default function WindowControls({ snapLayouts = false }: { snapLayouts?: boolean }) {
  const { t } = useTranslation();
  const [appWindow] = useState(() => getCurrentWindow());
  const [isMaximized, setIsMaximized] = useState(false);
  const maximizeButton = useRef<HTMLButtonElement>(null);
  const native = useSnapLayoutButton(maximizeButton, snapLayouts);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const update = async () => {
      const maximized = await appWindow.isMaximized();
      if (!disposed) setIsMaximized(maximized);
    };
    void update().catch(console.error);
    void appWindow
      .onResized(() => void update().catch(console.error))
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch(console.error);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [appWindow]);

  const maximizeLabel = t(isMaximized ? 'session.bar.windowRestore' : 'session.bar.windowMaximize');

  return (
    <div className="studio-window-controls">
      <button
        aria-label={t('session.bar.windowMinimize')}
        title={t('session.bar.windowMinimize')}
        onClick={() => void appWindow.minimize().catch(console.error)}
      >
        <Minus size={15} />
      </button>
      <button
        ref={maximizeButton}
        aria-label={maximizeLabel}
        title={maximizeLabel}
        className={
          [native.hovered && 'is-hovered', native.pressed && 'is-pressed'].filter(Boolean).join(' ') || undefined
        }
        onClick={() => void appWindow.toggleMaximize().catch(console.error)}
      >
        {isMaximized ? <Copy size={13} /> : <Square size={13} />}
      </button>
      <button
        aria-label={t('session.bar.windowClose')}
        title={t('session.bar.windowClose')}
        className="studio-window-close"
        onClick={() => void appWindow.close().catch(console.error)}
      >
        <X size={15} />
      </button>
    </div>
  );
}
