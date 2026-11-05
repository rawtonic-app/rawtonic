// The app's write queues for its own files, registered with `utils/quit.ts`
// in one place: Settings/Preferences (`settings.json`), albums and presets
// (`albums.json`, `presets.json`) and the edits' recipes (saved in
// `useEditorActions` itself, with the editor). Quitting, closing the window
// and every library switch drain them; the stores also drain their own queue
// for Save & Relaunch.
import { flushSettingsSaves } from '../store/useSettingsStore';
import { libraryChanges } from '../store/libraryFiles';
import { registerQuitFlush } from './quit';

/** Registers them with quitting once, at start-up (`main.tsx`); returns
 * their unregistration. A test that quits registers them itself, as tests
 * load modules in their own order. */
export function registerAppWrites(): () => void {
  const unregister = [
    // A preference changed just before quitting is still being saved.
    registerQuitFlush(() => flushSettingsSaves()),
    // So is an album or preset change, or a preset import still reading its
    // files (`libraryChanges` reports both, and a preset list whose file
    // could not be read).
    registerQuitFlush(() => libraryChanges.flush())
  ];
  return () => unregister.forEach((stop) => stop());
}
