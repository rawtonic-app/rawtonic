import { formatNumber } from '../../utils/formatNumber';
import {
  memo,
  useEffect,
  useId,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent
} from 'react';
import { useTranslation } from 'react-i18next';
import { capturePendingSliderClick, useEditorStore } from '../../store/useEditorStore';
import { endToolBefore, startValuePeek } from '../../hooks/useEditorActions';
import { selectOnFocus } from '../../utils/selectOnFocus';
import { MIN_SEND_INTERVAL_MS } from '../../hooks/useDragSend';
import { resolveKeySteps, stepOf, type KeySteps, type StepSet } from './sliderFeel';

export interface AdjustmentControlProps {
  label: string;
  /** Accessible name when the visible label repeats in another tool. */
  ariaLabel?: string;
  value: number;
  min?: number;
  max?: number;
  step?: number;
  defaultValue?: number;
  unit?: string;
  track?: 'temperature' | 'tint' | 'hue';
  /** Non-linear track: maps a value to its slider position and back
   * (e.g. Kelvin in mireds). Keys and the text field stay in value units. */
  scale?: {
    toTrack(value: number): number;
    fromTrack(position: number): number;
  };
  /** Keyboard steps (plain, Shift, Alt) of the slider and of the value field;
   * whole-unit sliders step like Lightroom's by default (sliderFeel.ts). */
  keys?: KeySteps;
  /** Shortest drag travel in points: on a shorter track a drag moves the value
   * as if the track were this long (HSL, as Lightroom's 200 pt). */
  minTravel?: number;
  disabled?: boolean;
  /** The field and slider show whole steps of a stored fraction (an old
   * recipe's 50.4 reads 50); the stored value stays until the control is
   * moved or reset. */
  wholeSteps?: boolean;
  /** Option (Alt) held during a drag shows a view of this control's effect
   * (the sharpening view, utils/sharpenView): called with true when it
   * should show and false when it ends (Option let go, or the drag). Option
   * then keeps the drag's speed instead of slowing it to a tenth. */
  optionView?: { onChange(shown: boolean): void; hint: string };
  /** Holding the label still shows the photo at `defaultValue` (UI-PEEK).
   * Only for sliders whose `onChange` changes the photo's recipe through
   * `setAdjustments` and nothing else: the peek runs it with the updates
   * caught (`adjustmentsAfter`), so any other write (a preference, a brush
   * setting) would really happen. */
  peek?: boolean;
  onChange(value: number): void;
  onInteraction?(active: boolean): void;
}

/** The thumb's width in the stylesheet (`.adjustment-track` thumb, 12 px at rest). */
const THUMB = 12;
/** A press this close to the thumb's centre grabs it without a jump. */
const GRAB = 8;
/** How long the label is held still before it shows the photo at the default. */
export const PEEK_HOLD_MS = 250;
/** A label press that moves less than this is still a hold, not a scrub. */
const SCRUB_START = 3;
// Recognize a following click while its history entry is still pending.
// Rendering always leaves drag mode at pointer-up.
const CLICK_SETTLE_MS = 500;

/**
 * How much of the track's own movement a drag makes per pixel when the
 * pointer is `distance` px above or below the track: all of it on and near
 * the track, then less the farther away (half at 48 px, a quarter at 96,
 * a tenth from 240), as on a scrubbing bar. A small hand movement then
 * makes a small change, without a modifier key (notes:
 * reference/slider-precision-2026-11-01.md).
 */
export function dragPrecision(distance: number) {
  const near = 24;
  return distance <= near ? 1 : Math.max(0.1, near / distance);
}

export function boundedValue(value: number, min: number, max: number, step: number) {
  const digits = Math.min(6, Math.max(0, -Math.floor(Math.log10(step))));
  return Number(Math.max(min, Math.min(max, value)).toFixed(digits));
}

/** The value as the field shows it: at most the fine (Alt) step's
 * precision, so a stored 1.6009 reads 1.6 and fits the field. */
export function shownValue(value: number, step: number) {
  const digits = Math.min(6, Math.max(0, -Math.floor(Math.log10(step / 10))));
  return formatNumber(value, digits, { trim: true });
}

/**
 * The value field `steps` fields after (or before) `field` in panel order:
 * Tab walks value field to value field as in Lightroom, so a column of
 * numbers is typed without the mouse (notes: reference/ux-windows-review-2026-11-16
 * UX4-01). The sliders between them are passed over too: a focused slider
 * leaves digits to the rating keys (`fieldOwnsKey`), so Tab must not land
 * there. Fields of closed tools and disabled ones are passed over.
 */
export function nextValueField(field: HTMLInputElement, steps: 1 | -1): HTMLInputElement | null {
  const root = field.closest('.studio-develop') ?? field.ownerDocument;
  const fields = Array.from(root.querySelectorAll<HTMLInputElement>('input.adjustment-value')).filter(
    (other) => other === field || (!other.disabled && !other.closest('details:not([open])'))
  );
  const at = fields.indexOf(field);
  return at < 0 ? null : (fields[at + steps] ?? null);
}

/** Independent desktop control: local feedback, engine updates as input
 * arrives, at most one per 8 ms. */
export default memo(function AdjustmentControl({
  label,
  ariaLabel,
  value,
  min = -100,
  max = 100,
  step = 1,
  defaultValue = 0,
  unit = '',
  track,
  scale,
  keys: keyOverride,
  minTravel = 0,
  disabled = false,
  wholeSteps = false,
  optionView,
  peek = false,
  onChange,
  onInteraction
}: AdjustmentControlProps) {
  const { t } = useTranslation();
  const id = useId();
  const [local, setLocal] = useState(value);
  const shown = (v: number) => shownValue(wholeSteps ? Math.round(v) : v, step);
  const [text, setText] = useState(shown(value));
  // A slider or label drag in progress: the row shows its pressed state.
  const [dragging, setDragging] = useState(false);
  // The pointer is over the row: Up/Down step the slider without a click.
  const [hovered, setHovered] = useState(false);
  const keySteps = resolveKeySteps(step, keyOverride);
  const trackRef = useRef<HTMLInputElement | null>(null);
  const active = useRef(false);
  const clickEnd = useRef<ReturnType<typeof setTimeout> | null>(null);
  const joinClick = useRef<(() => void) | null>(null);
  const trackMoved = useRef(false);
  const clickPosition = useRef({ x: 0, y: 0 });
  const frame = useRef<number | null>(null);
  const pending = useRef(value);
  const lastSent = useRef(value);
  const sentAt = useRef(-Infinity);
  const canceled = useRef(false);
  const callback = useRef(onChange);
  callback.current = onChange;
  const interactionCallback = useRef(onInteraction);
  interactionCallback.current = onInteraction;
  const optionViewCallback = useRef(optionView?.onChange);
  optionViewCallback.current = optionView?.onChange;
  // Whether the Option view shows (only while a drag is active).
  const viewShown = useRef(false);
  const showView = (shown: boolean) => {
    if (viewShown.current === shown || !optionViewCallback.current) return;
    viewShown.current = shown;
    optionViewCallback.current(shown);
  };
  // Option slows a drag to a tenth, except where it shows a view.
  const slowsDrag = (altKey: boolean) => altKey && !optionView;
  const scrub = useRef<{ x: number; value: number } | null>(null);
  // The held label's peek at the default: its timer, and whether it shows.
  const peekTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [peeking, setPeeking] = useState(false);
  const peekingRef = useRef(false);
  const stopPeek = () => {
    if (peekTimer.current !== null) clearTimeout(peekTimer.current);
    peekTimer.current = null;
    if (!peekingRef.current) return;
    peekingRef.current = false;
    setPeeking(false);
    endToolBefore();
  };
  const stopPeekRef = useRef(stopPeek);
  stopPeekRef.current = stopPeek;
  // A drag on the track: from the thumb without a jump, a pixel worth the
  // track's own movement (a tenth with Option, less away from the track).
  const fine = useRef<{
    x: number;
    centreY: number;
    value: number;
    perPixel: number;
  } | null>(null);
  // Slider positions: value units, or the scale's with 1000 steps.
  const trackMin = scale ? scale.toTrack(min) : min;
  const trackMax = scale ? scale.toTrack(max) : max;
  const trackStep = scale ? (trackMax - trackMin) / 1000 : step;
  const toTrack = (v: number) => (scale ? scale.toTrack(v) : v);
  const fromTrack = (p: number) => (scale ? scale.fromTrack(p) : p);
  // Where the default sits on the track (a small tick), when it is on it.
  const defaultPosition = (toTrack(defaultValue) - trackMin) / (trackMax - trackMin);
  const defaultAt =
    Number.isFinite(defaultPosition) && defaultPosition >= 0 && defaultPosition <= 1
      ? Math.round(defaultPosition * 10000) / 10000
      : null;
  useEffect(() => {
    if (!active.current) {
      setLocal(value);
      setText(shown(value));
      pending.current = value;
      lastSent.current = value;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `shown` follows wholeSteps
  }, [value, step, wholeSteps]);
  // Option pressed or let go while the drag holds still.
  useEffect(() => {
    if (!dragging || !optionView) return;
    const key = (event: globalThis.KeyboardEvent) => {
      if (event.key !== 'Alt') return;
      event.preventDefault();
      showView(event.type === 'keydown');
    };
    const hide = () => showView(false);
    window.addEventListener('keydown', key, true);
    window.addEventListener('keyup', key, true);
    window.addEventListener('blur', hide);
    return () => {
      window.removeEventListener('keydown', key, true);
      window.removeEventListener('keyup', key, true);
      window.removeEventListener('blur', hide);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- showView reads refs only
  }, [dragging, !!optionView]);
  useEffect(
    () => () => {
      if (viewShown.current) optionViewCallback.current?.(false);
      stopPeekRef.current();
      keyHold.current?.();
      if (clickEnd.current !== null) clearTimeout(clickEnd.current);
      if (frame.current !== null) cancelAnimationFrame(frame.current);
      if (active.current) {
        if (lastSent.current !== pending.current) callback.current(pending.current);
        interactionCallback.current?.(false);
      }
    },
    []
  );
  const flush = () => {
    if (frame.current !== null) cancelAnimationFrame(frame.current);
    frame.current = null;
    if (lastSent.current !== pending.current) {
      lastSent.current = pending.current;
      sentAt.current = performance.now();
      callback.current(pending.current);
    }
  };
  const update = (next: number, precision = step) => {
    if (!Number.isFinite(next)) return;
    pending.current = boundedValue(next, min, max, precision);
    setLocal(pending.current);
    setText(String(pending.current));
    // A drag's input goes to the engine as it arrives: waiting for the next
    // animation frame dropped one of two moves that fell into one (slow)
    // frame, ~1 in 6 at 60 Hz (notes: reference/slider-drag-nr.md). Faster
    // input (high-rate mice) still waits for the frame.
    if (performance.now() - sentAt.current >= MIN_SEND_INTERVAL_MS) flush();
    else if (frame.current === null) frame.current = requestAnimationFrame(flush);
  };
  const begin = () => {
    if (clickEnd.current !== null) clearTimeout(clickEnd.current);
    clickEnd.current = null;
    joinClick.current = null;
    setDragging(true);
    if (active.current) return;
    active.current = true;
    onInteraction?.(true);
  };
  const end = () => {
    if (clickEnd.current !== null) clearTimeout(clickEnd.current);
    clickEnd.current = null;
    joinClick.current = null;
    // The view ends first: the drag's last frame shows the photo.
    showView(false);
    flush();
    active.current = false;
    scrub.current = null;
    fine.current = null;
    setDragging(false);
    onInteraction?.(false);
  };
  const trackEnd = () => {
    if (!fine.current || trackMoved.current) {
      end();
      return;
    }
    const editorDrag = useEditorStore.getState().isSliderDragging;
    end();
    joinClick.current = editorDrag ? capturePendingSliderClick() : null;
    clickEnd.current = setTimeout(() => {
      clickEnd.current = null;
      joinClick.current = null;
    }, CLICK_SETTLE_MS);
  };
  const settleClick = useRef<() => void>(() => {});
  settleClick.current = () => {
    if (clickEnd.current !== null) clearTimeout(clickEnd.current);
    clickEnd.current = null;
    joinClick.current = null;
  };
  useEffect(() => {
    // A different control or command starts its own undo step, even if it
    // keeps keyboard focus on this track (labels prevent the focus move).
    const pointer = (event: globalThis.PointerEvent) => {
      if (event.target !== trackRef.current) settleClick.current();
    };
    const key = () => settleClick.current();
    window.addEventListener('pointerdown', pointer, true);
    window.addEventListener('keydown', key, true);
    return () => {
      window.removeEventListener('pointerdown', pointer, true);
      window.removeEventListener('keydown', key, true);
    };
  }, []);
  const reset = () => {
    if (disabled) return;
    update(defaultValue);
    end();
  };
  // A key press is an interaction, as a drag is: it is one history step
  // however long the key is held (auto-repeat), and the next press is the
  // next step, however soon it follows. It ends when that key comes up, or
  // with the window's focus.
  const keyHold = useRef<(() => void) | null>(null);
  const holdKey = (key: string) => {
    if (keyHold.current) return;
    const release = (event?: Event) => {
      if (event instanceof globalThis.KeyboardEvent && event.key !== key) return;
      window.removeEventListener('keyup', release, true);
      window.removeEventListener('blur', release);
      keyHold.current = null;
      interactionCallback.current?.(false);
    };
    keyHold.current = release;
    window.addEventListener('keyup', release, true);
    window.addEventListener('blur', release);
    interactionCallback.current?.(true);
  };
  // One key press on the slider: its steps, from the value shown.
  const stepBy = (direction: 1 | -1, set: StepSet, shift: boolean, alt: boolean, from = local) => {
    const delta = stepOf(set, shift, alt);
    update(from + direction * delta, Math.min(step, delta));
    flush();
  };
  const keys = (event: KeyboardEvent<HTMLInputElement>) => {
    if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    event.stopPropagation();
    holdKey(event.key);
    if (event.key === 'Home' || event.key === 'End') {
      update(event.key === 'Home' ? min : max);
      flush();
      return;
    }
    stepBy(['ArrowRight', 'ArrowUp'].includes(event.key) ? 1 : -1, keySteps.slider, event.shiftKey, event.altKey);
  };
  // Up/Down while the pointer rests on the row and no field or slider has
  // the focus: the hovered slider steps as a focused one does.
  const hoverKeys = useRef<(event: globalThis.KeyboardEvent) => void>(() => {});
  hoverKeys.current = (event) => {
    if (disabled || event.ctrlKey || event.metaKey || !['ArrowUp', 'ArrowDown'].includes(event.key)) return;
    const target = event.target as HTMLElement | null;
    // A focused field or slider (also another one's) owns the arrow keys.
    if (target?.closest('input, textarea, select, [contenteditable="true"]')) return;
    event.preventDefault();
    event.stopPropagation();
    holdKey(event.key);
    stepBy(event.key === 'ArrowUp' ? 1 : -1, keySteps.slider, event.shiftKey, event.altKey, pending.current);
  };
  useEffect(() => {
    if (!hovered) return;
    const key = (event: globalThis.KeyboardEvent) => hoverKeys.current(event);
    window.addEventListener('keydown', key, true);
    return () => window.removeEventListener('keydown', key, true);
  }, [hovered]);
  const scrubStart = (e: PointerEvent<HTMLLabelElement>) => {
    if (disabled || e.button !== 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    scrub.current = { x: e.clientX, value: local };
    // Option scrubs (or shows the control's view) at once.
    if (e.altKey) {
      begin();
      showView(true);
      return;
    }
    // A press held still shows the photo at the default (UI-PEEK); moving
    // starts the scrub. Nothing is sent until then; the row reads pressed.
    setDragging(true);
    if (peek)
      peekTimer.current = setTimeout(() => {
        peekTimer.current = null;
        // At its default already: nothing to show.
        if (!scrub.current || active.current || pending.current === defaultValue) return;
        const name = ariaLabel ?? label;
        if (startValuePeek(name, () => callback.current(defaultValue))) {
          peekingRef.current = true;
          setPeeking(true);
        }
      }, PEEK_HOLD_MS);
  };
  // The window losing focus ends a peek, as releasing does.
  useEffect(() => {
    if (!peeking) return;
    const blur = () => stopPeekRef.current();
    window.addEventListener('blur', blur);
    return () => window.removeEventListener('blur', blur);
  }, [peeking]);
  const commitText = () => {
    if (canceled.current) {
      canceled.current = false;
      pending.current = value;
      setLocal(value);
      setText(shown(value));
      active.current = false;
      return;
    }
    // A field left as it was sends nothing: a stored value outside the
    // slider's range (an older, wider range) must not be clamped by a focus.
    if (text === shown(pending.current)) {
      // untouched
    } else if (text.trim() && Number.isFinite(Number(text))) update(Number(text));
    else {
      pending.current = value;
      setLocal(value);
      setText(shown(value));
    }
    end();
  };
  return (
    <div
      className={`adjustment-control ${disabled ? 'is-disabled' : ''} ${dragging || peeking ? 'is-active' : ''} ${peeking ? 'is-peeking' : ''}`}
      onPointerEnter={() => setHovered(true)}
      onPointerLeave={() => setHovered(false)}
    >
      <label
        htmlFor={id}
        title={peek ? t('studio.adjustment.scrubHintPeek') : t('studio.adjustment.scrubHint')}
        onPointerDown={scrubStart}
        onPointerMove={(e) => {
          if (!scrub.current || peekingRef.current) return;
          if (!active.current) {
            if (Math.abs(e.clientX - scrub.current.x) < SCRUB_START) return;
            stopPeek();
            begin();
          }
          showView(e.altKey);
          update(
            fromTrack(
              toTrack(scrub.current.value) +
                (e.clientX - scrub.current.x) * trackStep * (e.shiftKey ? 10 : slowsDrag(e.altKey) ? 0.1 : 1)
            ),
            slowsDrag(e.altKey) ? step / 10 : step
          );
        }}
        onPointerUp={() => {
          if (!scrub.current) return;
          const scrubbed = active.current;
          scrub.current = null;
          stopPeek();
          if (!scrubbed) return setDragging(false);
          end();
          trackRef.current?.blur();
        }}
        onPointerCancel={() => {
          scrub.current = null;
          stopPeek();
          if (active.current) end();
          else setDragging(false);
        }}
        // Scrubbing owns the label gesture. Its synthetic click on the track
        // could otherwise give keyboard focus back after pointer-up.
        onClick={(e) => e.preventDefault()}
        onDoubleClick={reset}
      >
        {label}
      </label>
      <input
        id={id}
        ref={trackRef}
        className={`adjustment-track ${track || ''} ${defaultAt === null ? '' : 'has-default-tick'}`}
        style={defaultAt === null ? undefined : ({ '--default-at': defaultAt } as CSSProperties)}
        type="range"
        min={trackMin}
        max={trackMax}
        step={scale ? 'any' : step}
        value={toTrack(local)}
        disabled={disabled}
        onChange={(e) => update(fromTrack(Number(e.target.value)))}
        onPointerDown={(e) => {
          const box = e.currentTarget.getBoundingClientRect();
          const travel = box.width - THUMB;
          if (!disabled && e.button === 0 && travel > 0) {
            // The drag is ours, not the native slider's: pressing the thumb
            // no longer moves it to the pointer (a click changed the value by
            // up to half a thumb), and the drag can slow down.
            e.preventDefault();
            // Commit an edited number field before the drag reads pending.current.
            e.currentTarget.focus();
            e.currentTarget.setPointerCapture(e.pointerId);
            const left = (box.left || 0) + THUMB / 2;
            // The interaction starts before any value is sent: a jump to the
            // pointer is part of the drag (one history step), not a step of
            // its own before it.
            // Two distinct track positions are two clicks, not a reset gesture.
            if (
              clickEnd.current !== null &&
              Math.hypot(e.clientX - clickPosition.current.x, e.clientY - clickPosition.current.y) <= GRAB
            ) {
              joinClick.current?.();
            }
            clickPosition.current = { x: e.clientX, y: e.clientY };
            begin();
            trackMoved.current = false;
            showView(e.altKey);
            // pending, not local: focusing the track just committed a typed value
            // whose state has not rendered yet.
            let start = toTrack(pending.current);
            const thumb = left + ((start - trackMin) / (trackMax - trackMin)) * travel;
            // Option grabs the thumb wherever the press is, as on the label.
            if (!e.altKey && Math.abs(e.clientX - thumb) > GRAB) {
              // A press beside the thumb moves it there, as a slider does.
              const at = Math.max(0, Math.min(1, (e.clientX - left) / travel));
              start = trackMin + at * (trackMax - trackMin);
              update(fromTrack(start));
            }
            fine.current = {
              x: e.clientX,
              centreY: (box.top || 0) + box.height / 2,
              value: start,
              perPixel: (trackMax - trackMin) / Math.max(travel, minTravel)
            };
          } else begin();
        }}
        onPointerMove={(e) => {
          if (!fine.current) return;
          showView(e.altKey);
          const drag = fine.current;
          if (e.clientX !== drag.x) trackMoved.current = true;
          const factor = slowsDrag(e.altKey) ? 0.1 : dragPrecision(Math.abs(e.clientY - drag.centreY));
          // Each move adds its own distance at the precision of the moment:
          // moving away from the track never makes the value jump.
          drag.value = Math.max(
            trackMin,
            Math.min(trackMax, drag.value + (e.clientX - drag.x) * drag.perPixel * factor)
          );
          drag.x = e.clientX;
          update(fromTrack(drag.value), factor < 1 ? step / 10 : step);
        }}
        onPointerUp={(e) => {
          const dragged = fine.current && trackMoved.current;
          trackEnd();
          if (dragged) e.currentTarget.blur();
        }}
        onPointerCancel={end}
        onBlur={() => {
          settleClick.current();
          if (active.current) end();
        }}
        onKeyDown={keys}
        onDoubleClick={reset}
        aria-label={ariaLabel ?? label}
        title={optionView?.hint ?? t('studio.adjustment.trackHint')}
      />
      <input
        className="adjustment-value"
        aria-label={
          unit?.trim()
            ? t('studio.adjustment.valueWithUnit', { name: ariaLabel ?? label, unit: unit.trim() })
            : t('studio.adjustment.value', { name: ariaLabel ?? label })
        }
        inputMode="decimal"
        type="text"
        value={text}
        disabled={disabled}
        onMouseDown={selectOnFocus.onMouseDown}
        onMouseUp={selectOnFocus.onMouseUp}
        onFocus={(e) => {
          active.current = true;
          selectOnFocus.onFocus(e);
        }}
        onChange={(e) => setText(e.target.value)}
        onBlur={commitText}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
            e.preventDefault();
            holdKey(e.key);
            const typed = Number(text.replace(',', '.'));
            stepBy(
              e.key === 'ArrowUp' ? 1 : -1,
              keySteps.field,
              e.shiftKey,
              e.altKey,
              text.trim() && Number.isFinite(typed) ? typed : pending.current
            );
          }
          if (e.key === 'Tab' && !e.ctrlKey && !e.metaKey && !e.altKey) {
            const next = nextValueField(e.currentTarget, e.shiftKey ? -1 : 1);
            if (next) {
              e.preventDefault();
              // Leaving commits this field (blur); the next one selects its
              // value, so typed digits replace it.
              next.focus();
            }
          }
          if (e.key === 'Enter') e.currentTarget.blur();
          if (e.key === 'Escape') {
            canceled.current = true;
            e.currentTarget.blur();
          }
        }}
      />
    </div>
  );
});
