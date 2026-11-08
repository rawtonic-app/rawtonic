// The main window's side of the second window (second_window.rs): its state
// for the menu, and the Library's active photograph while the main window
// shows no live preview (Develop's frames reach it natively).
import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Invokes } from '../components/ui/AppProperties';
import { SECOND_WINDOW_EVENT, useSecondWindowStore, type SecondWindowState } from '../store/useSecondWindowStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';

/** Opens or closes the second window (Window ▸ Second Window, ⇧F). */
export function toggleSecondWindow(): void {
  const open = !useSecondWindowStore.getState().state.open;
  void invoke<SecondWindowState>(Invokes.SecondWindowSetOpen, { open })
    .then((state) => useSecondWindowStore.getState().set(state))
    .catch((error) => console.warn('Second window:', error));
}

export function useSecondWindow(): void {
  useEffect(() => {
    void invoke<SecondWindowState>(Invokes.SecondWindowState)
      .then((state) => useSecondWindowStore.getState().set(state))
      .catch(() => {});
    const unlisten = listen<SecondWindowState>(SECOND_WINDOW_EVENT, (event) =>
      useSecondWindowStore.getState().set(event.payload)
    );
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, []);

  const open = useSecondWindowStore((s) => s.state.open);
  const inLibrary = useUIStore((s) => s.activeView !== 'editor');
  const active = useLibraryStore((s) => s.libraryActivePath);
  useEffect(() => {
    if (open && inLibrary && active) void invoke(Invokes.SecondWindowFollow, { path: active }).catch(() => {});
  }, [open, inLibrary, active]);
}
