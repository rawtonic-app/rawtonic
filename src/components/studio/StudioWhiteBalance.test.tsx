import '../../i18n';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

import StudioWhiteBalance from './StudioWhiteBalance';
import { KELVIN_SCALE } from '../../utils/whiteBalance';

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

const flushFrames = () => act(() => new Promise((resolve) => requestAnimationFrame(() => resolve(0))));

describe('white balance', () => {
  it("shows the camera's As Shot Kelvin and tint", async () => {
    invoke.mockResolvedValue({ temperature: 6150, tint: 7 });
    render(
      <StudioWhiteBalance values={{}} imagePath="/s/a.NEF" disabled={false} onAuto={vi.fn()} onChange={vi.fn()} />
    );
    const temp = await screen.findByLabelText('White balance temperature value (K)');
    expect(invoke).toHaveBeenCalledWith('white_balance_info', {
      imagePath: '/s/a.NEF'
    });
    expect((temp as HTMLInputElement).value).toBe('6150');
    expect((screen.getByLabelText('White balance tint value') as HTMLInputElement).value).toBe('7');
    expect((screen.getByLabelText('White balance mode') as HTMLSelectElement).value).toBe('asShot');
  });

  it('puts the picker in the WB row beside the presets, as both reference editors do', async () => {
    invoke.mockResolvedValue({ temperature: 6150, tint: 7 });
    render(
      <StudioWhiteBalance
        values={{}}
        imagePath="/s/a.NEF"
        disabled={false}
        onAuto={vi.fn()}
        onChange={vi.fn()}
        picker={<button type="button" aria-label="White balance picker" />}
        fallback={<span>relative</span>}
      />
    );
    const mode = await screen.findByLabelText('White balance mode');
    const picker = screen.getByRole('button', { name: 'White balance picker' });
    // One row: the picker is no separate row above the presets.
    expect(mode.closest('.studio-inline')).toBe(picker.closest('.studio-inline'));
    // A press on the picker does not open the presets (not inside their label).
    expect(picker.closest('label')).toBeNull();
  });

  it('switches to presets and to Custom when a slider moves', async () => {
    invoke.mockResolvedValue({ temperature: 6150, tint: 7 });
    const onChange = vi.fn();
    const { rerender } = render(
      <StudioWhiteBalance values={{}} imagePath="/s/a.NEF" disabled={false} onAuto={vi.fn()} onChange={onChange} />
    );
    const mode = await screen.findByLabelText('White balance mode');
    fireEvent.change(mode, { target: { value: 'tungsten' } });
    expect(onChange).toHaveBeenLastCalledWith({
      whiteBalance: 'tungsten',
      wbTemperature: undefined,
      wbTint: undefined
    });
    rerender(
      <StudioWhiteBalance
        values={{ whiteBalance: 'tungsten' }}
        imagePath="/s/a.NEF"
        disabled={false}
        onAuto={vi.fn()}
        onChange={onChange}
      />
    );
    expect((screen.getByLabelText('White balance temperature value (K)') as HTMLInputElement).value).toBe('2850');
    // The track runs in mireds: its position for 4000 K.
    fireEvent.change(screen.getByLabelText('White balance temperature'), {
      target: { value: String(KELVIN_SCALE.toTrack(4000)) }
    });
    await flushFrames();
    expect(onChange).toHaveBeenLastCalledWith({
      whiteBalance: 'custom',
      wbTemperature: 4000,
      wbTint: 0
    });
    fireEvent.change(mode, { target: { value: 'asShot' } });
    expect(onChange).toHaveBeenLastCalledWith({
      whiteBalance: undefined,
      wbTemperature: undefined,
      wbTint: undefined
    });
  });

  it("Auto in the menu asks for the photo's Auto balance", async () => {
    invoke.mockResolvedValue({ temperature: 6150, tint: 7 });
    const onAuto = vi.fn();
    const onChange = vi.fn();
    render(
      <StudioWhiteBalance values={{}} imagePath="/s/a.NEF" disabled={false} onAuto={onAuto} onChange={onChange} />
    );
    const mode = await screen.findByLabelText('White balance mode');
    fireEvent.change(mode, { target: { value: 'auto' } });
    expect(onAuto).toHaveBeenCalledTimes(1);
    expect(onChange).not.toHaveBeenCalled();
  });

  it('falls back to the relative sliders without a camera calibration', async () => {
    invoke.mockResolvedValue(null);
    render(
      <StudioWhiteBalance
        values={{}}
        imagePath="/s/a.jpg"
        disabled={false}
        onAuto={vi.fn()}
        onChange={vi.fn()}
        fallback={<span>relative sliders</span>}
      />
    );
    await screen.findByText('relative sliders');
    expect(screen.queryByLabelText('White balance mode')).toBeNull();
  });

  it('maps Kelvin to mireds and back', () => {
    for (const k of [2000, 2850, 5500, 12000, 50000]) expect(KELVIN_SCALE.fromTrack(KELVIN_SCALE.toTrack(k))).toBe(k);
    expect(KELVIN_SCALE.toTrack(50000)).toBeGreaterThan(KELVIN_SCALE.toTrack(12000));
  });

  it("travels from As Shot as Lightroom's track does (4050 K: 6093 K at +30 pt, 8314 K at +60 pt)", () => {
    // Lightroom's Basic track is ~150 pt of thumb travel for 2000-50000 K.
    const perPoint = (KELVIN_SCALE.toTrack(50000) - KELVIN_SCALE.toTrack(2000)) / 150;
    const dragged = (from: number, points: number) =>
      KELVIN_SCALE.fromTrack(KELVIN_SCALE.toTrack(from) + points * perPoint);
    expect(Math.abs(dragged(4050, 30) / 6093 - 1)).toBeLessThan(0.003);
    expect(Math.abs(dragged(4050, 60) / 8314 - 1)).toBeLessThan(0.003);
    // The whole range stays on the track, in order.
    expect(KELVIN_SCALE.fromTrack(KELVIN_SCALE.toTrack(50000))).toBe(50000);
    let last = 0;
    for (const k of [2000, 3000, 5000, 9999, 10000, 10001, 20000, 50000]) {
      expect(KELVIN_SCALE.toTrack(k)).toBeGreaterThan(last);
      last = KELVIN_SCALE.toTrack(k);
    }
  });
});
