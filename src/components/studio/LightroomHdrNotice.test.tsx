// SPDX-License-Identifier: AGPL-3.0-or-later
import '../../i18n';
import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import LightroomHdrNotice, { isLightroomHdrEdit } from './LightroomHdrNotice';
import { useEditorStore } from '../../store/useEditorStore';

describe('the Lightroom HDR edit notice', () => {
  it('shows for HDREditMode 1 only', () => {
    expect(isLightroomHdrEdit({ lightroomHdr: { HDREditMode: '1', HDRMaxValue: '4' } })).toBe(true);
    expect(isLightroomHdrEdit({ lightroomHdr: { HDREditMode: '1.0' } })).toBe(true);
    expect(isLightroomHdrEdit({ lightroomHdr: { HDREditMode: '0' } })).toBe(false);
    expect(isLightroomHdrEdit({ lightroomHdr: { SDRBlend: '0.5' } })).toBe(false);
    expect(isLightroomHdrEdit({ exposure: 1 })).toBe(false);
    expect(isLightroomHdrEdit(null)).toBe(false);
  });

  beforeEach(() => useEditorStore.setState({ adjustments: {} as never }));

  it('renders the translated notice for an HDR edit and nothing otherwise', () => {
    render(<LightroomHdrNotice />);
    expect(screen.queryByRole('status')).toBeNull();
    useEditorStore.setState({ adjustments: { lightroomHdr: { HDREditMode: '1' } } as never });
    const { container } = render(<LightroomHdrNotice />);
    expect(container.textContent).toBe('HDR edit from Lightroom — shown and exported as SDR');
  });
});
