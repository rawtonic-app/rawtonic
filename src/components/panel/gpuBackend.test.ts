// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from 'vitest';
import i18n from 'i18next';
import en from '../../i18n/locales/en.json';
import { GpuBackendStatus, backendNames, gpuNoticeText } from './gpuBackend';

await i18n.init({ lng: 'en', resources: { en: { translation: en } }, interpolation: { escapeValue: false } });
const t = i18n.t;

const status = (over: Partial<GpuBackendStatus>): GpuBackendStatus => ({
  inUse: 'dx12',
  adapter: 'GPU',
  step: 'fellBack',
  avoided: 'vulkan',
  crashed: 'vulkan',
  crashedOn: '2026-11-06',
  notice: null,
  ...over
});

describe('GPU backend fallback notice', () => {
  it('names the backends', () => {
    expect(backendNames('vulkan,dx12', t)).toBe('Vulkan / DirectX 12');
    expect(backendNames('', t)).toBe('');
  });

  it('names the backend that crashed and the one used now', () => {
    const once = gpuNoticeText(
      status({ notice: { kind: 'fellBack', crashed: 'vulkan', stage: 'device', avoid: 'vulkan', kept: false } }),
      t
    );
    expect(once).toContain('with Vulkan last time');
    expect(once).toContain('uses DirectX 12 for now');
    const kept = gpuNoticeText(
      status({ notice: { kind: 'fellBack', crashed: 'vulkan', stage: 'compile', avoid: 'vulkan', kept: true } }),
      t
    );
    expect(kept).toContain('keeps using DirectX 12');
    expect(kept).toContain('Use automatic again');
  });

  it('says when no GPU started and when the crashed backend is not known', () => {
    const none = gpuNoticeText(
      status({ inUse: null, notice: { kind: 'fellBack', crashed: '', stage: 'unknown', avoid: '', kept: false } }),
      t
    );
    expect(none).toContain('uses no graphics processor now');
  });

  it('mentions an OpenGL preference and says nothing without a notice', () => {
    expect(gpuNoticeText(status({ notice: { kind: 'openGlPreference' } }), t)).toContain('OpenGL');
    expect(gpuNoticeText(status({ notice: null }), t)).toBeNull();
  });
});
