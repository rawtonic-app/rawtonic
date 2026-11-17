/**
 * The fit view shows the export's colour noise reduction once the app has
 * built it from the full-resolution photo (src-tauri/src/color_noise_view.rs).
 * On the native display the app shows that frame itself; previews in the
 * webview (Linux) are answers to the editor's requests, so the app reports
 * the correction (`color-noise-view-ready`) and the editor asks for the
 * frame again. `render(path)` runs for each report; returns the stop.
 */
import { listen } from '@tauri-apps/api/event';

export function onColorNoiseViewReady(render: (path: string) => void): () => void {
  // Without the native app (tests, `npm run dev`) there is no event.
  const unlisten = listen<{ path: string }>('color-noise-view-ready', (event) => {
    render(event.payload.path);
  }).catch(() => undefined);
  return () => void unlisten.then((stop) => stop?.());
}
