import { memo, useRef, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { useEditorStore } from '../../store/useEditorStore';
import { useEditorStoreDom } from '../../hooks/useEditorStoreDom';
import { COLOR_SPACES, COLOR_SPACE_NAMES } from '../../utils/exportRecipes';
import { clippingChannelColour, histogramClipping } from '../../utils/clippingWarnings';
import {
  formatAperture,
  formatFocalLength,
  formatIso,
  formatShutter,
  parseExposureTime
} from '../../utils/captureInfo';
import type { OutputColorSpace } from '../ui/ExportImportProperties';

export function histogramBins(channel: unknown): number[] {
  const bins = Array.isArray(channel) ? channel : (channel as { data?: unknown })?.data;
  return Array.isArray(bins) ? bins.map((value) => (Number.isFinite(value) ? Math.max(0, value) : 0)) : [];
}

type ClippingSide = 'shadows' | 'highlights';

/** One fixed-size control above the plot that switches that
 * side's warning (left: shadows; right: highlights). `lit` is the
 * histogram's own verdict: this side clips, whatever the overlay shows. */
function ClippingCorner({
  side,
  on,
  lit,
  colour,
  label,
  title,
  onToggle
}: {
  side: ClippingSide;
  on: boolean;
  lit: boolean;
  /** The clipping channels' colour of a lit triangle; null: neutral. */
  colour: string | null;
  label: string;
  title: string;
  onToggle(): void;
}) {
  return (
    <button
      type="button"
      className={`studio-clip-corner studio-clip-${side}${on ? ' is-on' : ''}${lit ? ' is-lit' : ''}`}
      aria-label={label}
      aria-pressed={on}
      title={title}
      data-clipping={lit || undefined}
      style={lit && colour ? ({ '--clip-channel': colour } as CSSProperties) : undefined}
      onClick={(event) => {
        event.stopPropagation();
        onToggle();
      }}
      onKeyDown={(event) => {
        // Space otherwise reaches the global zoom shortcut. Keep native
        // button activation for Space/Enter and leave J available globally.
        if (event.key === ' ' || event.key === 'Enter') event.stopPropagation();
      }}
    >
      <svg viewBox="0 0 12 12" aria-hidden="true">
        <polygon className="studio-clip-corner-mark" points="6,1 11.5,11 0.5,11" />
      </svg>
    </button>
  );
}

const CHANNELS = ['red', 'green', 'blue'] as const;
const selectHistogram = (s: ReturnType<typeof useEditorStore.getState>) => s.histogram;

/** The `d` of each channel's filled outline in the 256 × 84 plot (square-root
 * scale against the tallest bin of the three), or null for an empty channel. */
export function histogramPaths(histogram: unknown): (string | null)[] {
  const data = histogram as Record<string, unknown> | null;
  const bins = CHANNELS.map((channel) => histogramBins(data?.[channel]));
  const max = Math.max(1, ...bins.flat());
  return bins.map((values) =>
    values.length
      ? `M0 84 ${values.map((v, i) => `L${(i * 256) / Math.max(1, values.length - 1)} ${84 - Math.sqrt(Math.max(0, v) / max) * 80}`).join(' ')} L256 84 Z`
      : null
  );
}

/** The histogram's curves, drawn from the store without rendering: scope
 * updates arrive ten times a second during drags (useEditorStoreDom). */
const HistogramPlot = memo(function HistogramPlot() {
  const { t } = useTranslation();
  const svg = useRef<SVGSVGElement>(null);
  const paths = useRef<(SVGPathElement | null)[]>([]);
  useEditorStoreDom(
    selectHistogram,
    (histogram) => {
      histogramPaths(histogram).forEach((d, i) => {
        const path = paths.current[i];
        if (!path) return;
        if (d) path.setAttribute('d', d);
        else path.removeAttribute('d');
      });
      svg.current?.setAttribute(
        'aria-label',
        histogram ? t('studio.histogram.graphAria') : t('studio.histogram.graphEmpty')
      );
    },
    [t]
  );
  return (
    <div className="studio-histogram-plot">
      <svg ref={svg} viewBox="0 0 256 84" preserveAspectRatio="none" role="img">
        {[64, 128, 192].map((x) => (
          <line key={x} x1={x} x2={x} y1="0" y2="84" stroke="currentColor" opacity=".15" />
        ))}
        {CHANNELS.map((channel, i) => (
          <path
            key={channel}
            ref={(el) => {
              paths.current[i] = el;
            }}
            className={`histogram-${channel}`}
          />
        ))}
      </svg>
    </div>
  );
});

export default memo(function StudioHistogram() {
  const { t } = useTranslation();
  // Primitives only: a scope update re-renders this panel when a clipping
  // triangle changes, not for every new histogram (the plot draws itself).
  const shadowsLit = useEditorStore((s) => histogramClipping(s.histogram).shadows);
  const highlightsLit = useEditorStore((s) => histogramClipping(s.histogram).highlights);
  const shadowsColour = useEditorStore((s) => clippingChannelColour(s.histogram, 'shadows'));
  const highlightsColour = useEditorStore((s) => clippingChannelColour(s.histogram, 'highlights'));
  const image = useEditorStore((s) => s.selectedImage);
  const clipHighlights = useEditorStore((s) => s.clipHighlights);
  const clipShadows = useEditorStore((s) => s.clipShadows);
  const proof = useEditorStore((s) => s.softProof);
  const proofSpace = useEditorStore((s) => s.softProofSpace);
  const proofWarning = useEditorStore((s) => s.softProofWarning);
  const setEditor = useEditorStore((s) => s.setEditor);
  const clipped = { shadows: shadowsLit, highlights: highlightsLit };
  return (
    <section className="studio-histogram" aria-label={t('studio.histogram.aria')}>
      <div className="studio-clip-corners">
        <ClippingCorner
          side="shadows"
          on={clipShadows}
          lit={clipped.shadows}
          colour={shadowsColour}
          label={clipped.shadows ? t('studio.histogram.shadowClippingLit') : t('studio.histogram.shadowClipping')}
          title={t('studio.histogram.shadowClippingTitle', {
            shortcut: 'J'
          })}
          onToggle={() =>
            setEditor({
              clipShadows: !useEditorStore.getState().clipShadows
            })
          }
        />
        <div className="studio-section-caption">
          <span>{t('studio.histogram.title')}</span>
          <span className="studio-histogram-toggles">
            <button
              type="button"
              className="studio-clipping-toggle"
              aria-pressed={proof}
              title={t('studio.histogram.proofTitle', { shortcut: 'Shift+S' })}
              onClick={() => setEditor({ softProof: !proof })}
            >
              {t('studio.histogram.proof')}
            </button>
          </span>
        </div>
        <ClippingCorner
          side="highlights"
          on={clipHighlights}
          lit={clipped.highlights}
          colour={highlightsColour}
          label={
            clipped.highlights ? t('studio.histogram.highlightClippingLit') : t('studio.histogram.highlightClipping')
          }
          title={t('studio.histogram.highlightClippingTitle', {
            shortcut: 'J'
          })}
          onToggle={() =>
            setEditor({
              clipHighlights: !useEditorStore.getState().clipHighlights
            })
          }
        />
      </div>
      {proof && (
        <div className="studio-proof-line">
          <select
            aria-label={t('studio.histogram.proofSpace')}
            value={proofSpace}
            onChange={(e) => setEditor({ softProofSpace: e.target.value as OutputColorSpace })}
          >
            {COLOR_SPACES.map((space) => (
              <option key={space} value={space}>
                {COLOR_SPACE_NAMES[space]}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="studio-clipping-toggle"
            aria-pressed={proofWarning}
            title={t('studio.histogram.gamutTitle')}
            onClick={() => setEditor({ softProofWarning: !proofWarning })}
          >
            {t('studio.histogram.gamut')}
          </button>
        </div>
      )}
      <HistogramPlot />
      <div className="studio-exif-line">
        {image ? (
          <>
            <span>
              {formatIso(image.exif?.ISO || image.exif?.PhotographicSensitivity) ?? (image.isRaw ? 'RAW' : 'RGB')}
            </span>
            <span>{formatAperture(image.exif?.FNumber) ?? '—'}</span>
            <span>{formatShutter(parseExposureTime(image.exif?.ExposureTime)) ?? '—'}</span>
            <span>{formatFocalLength(image.exif?.FocalLength) ?? `${image.width} × ${image.height}`}</span>
          </>
        ) : (
          <span>{t('studio.histogram.selectPhoto')}</span>
        )}
      </div>
    </section>
  );
});
