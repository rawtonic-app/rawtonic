import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy, Minus, Square, X } from "lucide-react";

/** Linux window controls share the session bar instead of adding a second row. */
export default function WindowControls() {
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
    void appWindow.onResized(() => void update().catch(console.error)).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    }).catch(console.error);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [appWindow]);

  return (
    <div className="studio-window-controls">
      <button
        aria-label="Minimize window"
        title="Minimize window"
        onClick={() => void appWindow.minimize().catch(console.error)}
      >
        <Minus size={15} />
      </button>
      <button
        aria-label={isMaximized ? "Restore window" : "Maximize window"}
        title={isMaximized ? "Restore window" : "Maximize window"}
        onClick={() => void appWindow.toggleMaximize().catch(console.error)}
      >
        {isMaximized ? <Copy size={13} /> : <Square size={13} />}
      </button>
      <button
        aria-label="Close window"
        title="Close window"
        className="studio-window-close"
        onClick={() => void appWindow.close().catch(console.error)}
      >
        <X size={15} />
      </button>
    </div>
  );
}
