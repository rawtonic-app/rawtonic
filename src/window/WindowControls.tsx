import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Copy, Minus, Square, X } from 'lucide-react';

/** Windows and Linux window controls share the session bar instead of adding a second row. */
export default function WindowControls() {
  const { t } = useTranslation();
  const [appWindow] = useState(() => getCurrentWindow());
  const [isMaximized, setIsMaximized] = useState(false);

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
        aria-label={maximizeLabel}
        title={maximizeLabel}
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
