// SPDX-License-Identifier: AGPL-3.0-or-later
import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';

export interface UpdateInfo { version: string; releaseUrl: string; appImage: boolean }
interface UpdateState {
  update: UpdateInfo | null;
  checking: boolean;
  checked: boolean;
  failed: boolean;
  started: boolean;
  check(manual: boolean): Promise<void>;
  dismiss(): void;
}
export const useUpdateStore = create<UpdateState>((set, get) => ({
  update: null, checking: false, checked: false, failed: false, started: false,
  check: async (manual) => {
    if (get().checking || (!manual && get().started)) return;
    set({ checking: true, started: true, checked: false, failed: false });
    try {
      const update = await invoke<UpdateInfo | null>(Invokes.CheckUpdates, { manual });
      set({ update, checked: manual });
    } catch (error) {
      console.debug('Update check failed:', error);
      set({ failed: manual });
    } finally {
      set({ checking: false });
    }
  },
  dismiss: () => set({ update: null }),
}));
