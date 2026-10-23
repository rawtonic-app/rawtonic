import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useEditorStore } from '../store/useEditorStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useSnapshotStore, type Snapshot } from '../store/useSnapshotStore';
import { endBeforePeek, startBeforePeek } from '../hooks/useEditorActions';
import { requestSplitBefore } from '../hooks/useImageProcessing';
import { INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { originalPreviewAdjustments } from './beforeAfter';
import { beforeSnapshot, beforeSnapshotAdjustments, compareWithSnapshot } from './beforeSnapshot';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve(true)) }));

const PHOTO = '/p.dng';
const subject = { id: 'subject', type: 'ai-subject', visible: true, mode: 'additive', parameters: { maskDataBase64: 'old' } };
const radial = {
  id: 'radial',
  type: 'radial',
  visible: true,
  mode: 'additive',
  parameters: { centerX: 100, centerY: 200, radiusX: 50, radiusY: 50, rotation: 0, feather: 0.5 },
};
const layer = (subMasks: unknown[]) => ({ id: 'layer', name: 'Layer', visible: true, invert: false, opacity: 100, adjustments: {}, subMasks });

/** A snapshot framed and developed as the edit, with a drawn mask and an AI
 * mask whose id the edit reuses for a newer bitmap. */
const snapshot: Snapshot = {
  id: 's',
  name: 'Turned',
  createdAt: '2026-10-23T08:00:00.000Z',
  adjustments: {
    ...INITIAL_ADJUSTMENTS,
    exposure: 1,
    masks: [layer([subject, radial])],
  } as unknown as Adjustments,
};

beforeEach(() => {
  vi.mocked(invoke).mockClear();
  useSettingsStore.setState({ osPlatform: 'macos', appSettings: { useWgpuRenderer: true } as never });
  useSnapshotStore.setState({ path: PHOTO, snapshots: [snapshot] });
  useEditorStore.setState({
    selectedImage: { path: PHOTO, isReady: true, isDecoded: true, isRaw: true, width: 6000, height: 4000, aiDenoise: 0 } as never,
    adjustments: { ...INITIAL_ADJUSTMENTS, masks: [layer([{ ...subject, parameters: { maskDataBase64: 'new' } }])] } as never,
    splitView: true,
    showOriginal: false,
    previewOverride: null,
    patchesSentToBackend: new Set<string>(),
    splitBeforeShows: null,
  });
  compareWithSnapshot(snapshot);
});

afterEach(() => {
  endBeforePeek();
  compareWithSnapshot(null);
  useEditorStore.setState({ splitView: false });
});

it('gives the snapshot AI bitmaps ids of their own, the same for each call', () => {
  const before = beforeSnapshotAdjustments()!;
  const ai = before.masks[0].subMasks[0];
  expect(ai.parameters.maskDataBase64).toBe('old');
  // The edit keeps 'subject' for its newer bitmap: the native side must not
  // take one for the other.
  expect(ai.id).not.toBe('subject');
  expect(beforeSnapshotAdjustments()!.masks[0].subMasks[0].id).toBe(ai.id);
});

it('is the before only while it frames the photograph as the edit does', () => {
  expect(beforeSnapshot()?.id).toBe('s');
  // The edit turned a quarter: the snapshot's spots, drawn masks, relight
  // and face retouch belong to its own framing, so the before is the
  // unedited photograph again.
  useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, orientationSteps: 1 } as never });
  expect(beforeSnapshot()).toBeNull();
  expect(originalPreviewAdjustments(useEditorStore.getState().adjustments, beforeSnapshotAdjustments()).exposure).toBe(0);
  // Framed alike again, it is compared with again.
  useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, orientationSteps: 0 } as never });
  expect(beforeSnapshot()?.id).toBe('s');
});

it('is the before only while it develops the RAW as the edit does', () => {
  // AI Denoise on in the edit: the snapshot would need a source of its own.
  useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, aiDenoise: 40 } as never });
  expect(beforeSnapshot()).toBeNull();
  // The multi-scale colour noise filter (decoded without the chroma clean-up): the same.
  useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, colorNoiseVersion: 9 } as never });
  expect(beforeSnapshot()).toBeNull();
  useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS } as never });
  expect(beforeSnapshot()?.id).toBe('s');
});

it('renders the split before and the held peek from the snapshot', () => {
  requestSplitBefore(1200);
  const call = vi.mocked(invoke).mock.calls.find(([command]) => command === Invokes.RenderSplitBefore);
  expect(call?.[1]).toMatchObject({ path: PHOTO, jsAdjustments: { exposure: 1 } });
  expect(call?.[1]).not.toHaveProperty('sourceDevelop');
  expect(startBeforePeek()).toBe(true);
  expect(useEditorStore.getState().previewOverride?.exposure).toBe(1);
});

it('names the snapshot whose before is in place, not one still on its way', async () => {
  const other: Snapshot = { ...snapshot, id: 'o', name: 'Other', adjustments: { ...snapshot.adjustments, exposure: -1 } };
  useSnapshotStore.setState({ snapshots: [snapshot, other] });
  const place: Array<(placed: boolean) => void> = [];
  vi.mocked(invoke).mockImplementation(() => new Promise((resolve) => place.push(resolve as never)));
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
  try {
    requestSplitBefore(1200);
    // Another snapshot chosen while the first one renders: it waits its turn.
    compareWithSnapshot(other);
    requestSplitBefore(1200);
    expect(useEditorStore.getState().splitBeforeShows).toBeNull();
    place[0](true);
    await settle();
    // The first one's before is on screen, under its own name.
    expect(useEditorStore.getState().splitBeforeShows).toBe('s');
    place[1](true);
    await settle();
    expect(useEditorStore.getState().splitBeforeShows).toBe('o');
  } finally {
    vi.mocked(invoke).mockImplementation(() => Promise.resolve(true));
  }
});

it('sends the snapshot bitmaps with the first split request only', async () => {
  const bitmap = () => {
    const calls = vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.RenderSplitBefore);
    const payload = calls.at(-1)?.[1] as { jsAdjustments: Adjustments };
    return payload.jsAdjustments.masks[0].subMasks[0].parameters.maskDataBase64;
  };
  requestSplitBefore(1200);
  expect(bitmap()).toBe('old');
  await new Promise((resolve) => setTimeout(resolve, 0));
  requestSplitBefore(1200);
  // The native side keeps it under the snapshot's own id.
  expect(bitmap()).toBeNull();
  // The snapshot as compared keeps its bitmap for the next photograph load.
  expect(beforeSnapshotAdjustments()!.masks[0].subMasks[0].parameters.maskDataBase64).toBe('old');
});
