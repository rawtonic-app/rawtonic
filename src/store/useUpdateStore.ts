// SPDX-License-Identifier: AGPL-3.0-or-later
import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';

export interface UpdateInfo { version: string; releaseUrl: string; appImage: boolean }
interface UpdateState {
  update: UpdateInfo | null;
  checking: boolean;
  checked: boolean;
  started: boolean;
  check(manual: boolean): Promise<void>;
  dismiss(): void;
}
export const useUpdateStore = create<UpdateState>((set, get) => ({
  update: null, checking: false, checked: false, started: false,
  check: async (manual) => {
    if (get().checking || (!manual && get().started)) return;
    set({ checking: true, started: true, checked: false });
    try {
      const update = await invoke<UpdateInfo | null>(Invokes.CheckUpdates, { manual });
      set({ update, checked: manual });
    } catch (error) {
      console.debug('Update check failed:', error);
    } finally {
      set({ checking: false });
    }
  },
  dismiss: () => set({ update: null }),
}));
