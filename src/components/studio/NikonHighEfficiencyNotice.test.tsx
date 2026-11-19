import '../../i18n';
import { render, screen } from '@testing-library/react';
import { expect, it } from 'vitest';
import NikonHighEfficiencyNotice from './NikonHighEfficiencyNotice';

it('shows the codec and both alternatives above a High Efficiency preview', () => {
  const { rerender } = render(<NikonHighEfficiencyNotice visible />);
  const notice = screen.getByRole('status');
  expect(notice.textContent).toContain('TicoRAW) is patent-protected');
  expect(notice.textContent).toContain('Adobe DNG Converter');
  expect(notice.textContent).toContain('Lossless compressed RAW');
  rerender(<NikonHighEfficiencyNotice />);
  expect(screen.queryByRole('status')).toBeNull();
});
