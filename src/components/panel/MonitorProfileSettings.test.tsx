// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import MonitorProfileSettings, { type MonitorStatus } from './MonitorProfileSettings';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => undefined)) }));
vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, unknown>) => (values ? `${key} ${JSON.stringify(values)}` : key)
  })
}));

const status = (over: Partial<MonitorStatus>): MonitorStatus => ({
  mode: 'srgb',
  profile: null,
  location: null,
  fromOverride: false,
  notice: null,
  overrideAvailable: false,
  ...over
});

describe('MonitorProfileSettings', () => {
  afterEach(cleanup);

  it('asks the app once, not on every render', async () => {
    invoke.mockReset();
    invoke.mockResolvedValue(status({ mode: 'converted', profile: 'Studio EIZO', overrideAvailable: true }));
    render(<MonitorProfileSettings />);
    expect((await screen.findByTestId('monitor-profile-summary')).textContent).toContain('Studio EIZO');
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it('names the profile the preview is converted into', async () => {
    render(
      <MonitorProfileSettings
        status={() => Promise.resolve(status({ mode: 'converted', profile: 'Studio EIZO', overrideAvailable: true }))}
      />
    );
    expect((await screen.findByTestId('monitor-profile-summary')).textContent).toContain('Studio EIZO');
    expect(screen.getByText('settings.monitorProfile.choose')).toBeTruthy();
    expect(screen.queryByText('settings.monitorProfile.useSystem')).toBeNull();
  });

  it('offers no choice where the system manages colour', async () => {
    render(<MonitorProfileSettings status={() => Promise.resolve(status({ mode: 'system' }))} />);
    expect(await screen.findByText('settings.monitorProfile.system')).toBeTruthy();
    expect(screen.queryByText('settings.monitorProfile.choose')).toBeNull();
  });

  it('chooses a profile and goes back to the system one', async () => {
    const choose = vi.fn((path: string | null) =>
      Promise.resolve(
        status({
          mode: path ? 'converted' : 'srgb',
          profile: path ? 'Mine' : null,
          location: path,
          fromOverride: path !== null,
          overrideAvailable: true
        })
      )
    );
    render(
      <MonitorProfileSettings
        status={() => Promise.resolve(status({ overrideAvailable: true }))}
        choose={choose}
        pick={() => Promise.resolve('C:/mine.icc')}
      />
    );
    fireEvent.click(await screen.findByText('settings.monitorProfile.choose'));
    await waitFor(() => expect(choose).toHaveBeenCalledWith('C:/mine.icc'));
    fireEvent.click(await screen.findByText('settings.monitorProfile.useSystem'));
    await waitFor(() => expect(choose).toHaveBeenLastCalledWith(null));
  });

  it('says why a profile was refused', async () => {
    render(
      <MonitorProfileSettings
        status={() => Promise.resolve(status({ overrideAvailable: true }))}
        choose={() => Promise.reject('not an RGB display profile')}
        pick={() => Promise.resolve('C:/gray.icc')}
      />
    );
    fireEvent.click(await screen.findByText('settings.monitorProfile.choose'));
    expect((await screen.findByRole('alert')).textContent).toContain('not an RGB display profile');
  });
});
