import '../../i18n';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

import StudioFaceRetouch, { faceRetouchGeometry, faceRetouchIsCurrent } from './StudioFaceRetouch';
import { useEditorStore } from '../../store/useEditorStore';
import { INITIAL_ADJUSTMENTS, type FaceRetouch } from '../../utils/adjustments';
import { GEOMETRY_KEYS, ORIENTATION_KEYS } from '../../utils/geometryKeys';

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

describe('face retouch models', () => {
  it('downloads the face models only when asked (PRIVACY.md)', async () => {
    invoke.mockImplementation((_command: string, args: { download?: boolean }) =>
      args.download
        ? Promise.resolve({ frame: 'f', faces: [] })
        : Promise.reject({ code: 'models_missing', message: 'face-models-missing' })
    );
    useEditorStore.setState({
      selectedImage: { path: '/s/Capture/a.CR3', isReady: true } as never,
      adjustments: { ...INITIAL_ADJUSTMENTS, faceRetouch: undefined }
    });
    render(<StudioFaceRetouch onInteraction={() => {}} />);

    const button = await screen.findByRole('button', { name: 'Download face models' });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][1]).toMatchObject({ download: false });

    fireEvent.click(button);
    await waitFor(() => expect(screen.getByText('No faces found in this photo.')).toBeTruthy());
    expect(invoke.mock.calls.at(-1)?.[1]).toMatchObject({ download: true });
  });
});

describe('face retouch frames', () => {
  const legacyRecipe = () => {
    const adjustments = { ...INITIAL_ADJUSTMENTS, fringeVersion: 1, lensVignetteAmount: 30 };
    // Faces found before frame version 2: the panel kept every geometry key.
    const geometry = JSON.stringify(faceRetouchGeometry(adjustments, [...ORIENTATION_KEYS, ...GEOMETRY_KEYS]));
    const faceRetouch = {
      settings: { blemishes: 40 },
      faces: [{ id: 'f1', kept: [3], added: [7], settings: {}, blemishes: [], box: [0, 0, 10, 10] }],
      frame: '{"rotation":0}',
      geometry
    } as unknown as FaceRetouch;
    return { ...adjustments, faceRetouch };
  };

  it('keeps an unchanged older retouch as it is, without finding faces again', async () => {
    invoke.mockResolvedValue({ frame: '2:{}', faces: [] });
    const adjustments = legacyRecipe();
    useEditorStore.setState({ selectedImage: { path: '/s/a.CR3', isReady: true } as never, adjustments });
    render(<StudioFaceRetouch onInteraction={() => {}} />);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(invoke).not.toHaveBeenCalled();
    expect(useEditorStore.getState().adjustments.faceRetouch).toBe(adjustments.faceRetouch);
    expect(faceRetouchIsCurrent(adjustments.faceRetouch, adjustments)).toBe(true);
    // Its rule: a vignetting change makes it stale, as in the engine.
    expect(faceRetouchIsCurrent(adjustments.faceRetouch, { ...adjustments, lensVignetteAmount: 50 })).toBe(false);
  });

  it('keeps a newer retouch when only colour corrections change', () => {
    const adjustments = { ...INITIAL_ADJUSTMENTS, fringeVersion: 1 };
    const faceRetouch = {
      settings: {},
      faces: [],
      frame: '2:{}',
      geometry: JSON.stringify(faceRetouchGeometry(adjustments))
    } as unknown as FaceRetouch;
    expect(faceRetouchIsCurrent(faceRetouch, { ...adjustments, fringeVersion: 2, lensVignetteAmount: 50 })).toBe(true);
    expect(faceRetouchIsCurrent(faceRetouch, { ...adjustments, rotation: 3 })).toBe(false);
  });
});

describe('eyes and teeth on faces found before them', () => {
  it('waits for the user to find the eyes and mouth, then stores them with the faces', async () => {
    const eyes = [
      {
        iris: [5, 5, 2],
        lid: [
          [0, 0],
          [9, 0],
          [9, 9]
        ]
      },
      null
    ];
    invoke.mockResolvedValue({ frame: '2:{}', faces: [{ id: 'face-1', box: [0, 0, 10, 10], eyes, mouth: null }] });
    const adjustments = { ...INITIAL_ADJUSTMENTS };
    const faceRetouch = {
      settings: { blemishes: 40 },
      faces: [{ id: 'face-1', kept: [3], settings: {}, blemishes: [], box: [1, 0, 10, 10], enabled: true }],
      frame: '2:{}',
      geometry: JSON.stringify(faceRetouchGeometry(adjustments))
    } as unknown as FaceRetouch;
    useEditorStore.setState({
      selectedImage: { path: '/s/b.CR3', isReady: true } as never,
      adjustments: { ...adjustments, faceRetouch }
    });
    render(<StudioFaceRetouch onInteraction={() => {}} />);
    await new Promise((resolve) => setTimeout(resolve, 20));
    // Opening the tool runs nothing; the sliders wait.
    expect(invoke).not.toHaveBeenCalled();
    expect((screen.getByRole('slider', { name: 'Iris (Both)' }) as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole('slider', { name: 'Teeth whiten' }) as HTMLInputElement).disabled).toBe(true);

    fireEvent.click(screen.getByRole('button', { name: 'Find eyes and teeth' }));
    await waitFor(() => expect(useEditorStore.getState().adjustments.faceRetouch?.faces[0].eyes).toEqual(eyes));
    expect(invoke.mock.calls[0][1]).toMatchObject({ download: false });
    // Then the eye and teeth parser, installed models only.
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    expect(invoke.mock.calls[1][1]).toMatchObject({ eyesTeeth: true, downloadEyesTeeth: false });
    const stored = useEditorStore.getState().adjustments.faceRetouch!;
    expect(stored.faces[0]).toMatchObject({ kept: [3], mouth: null });
    expect(stored.settings).toEqual({ blemishes: 40 });
    await waitFor(() =>
      expect((screen.getByRole('slider', { name: 'Iris (Both)' }) as HTMLInputElement).disabled).toBe(false)
    );
  });
});

describe('a failed search for eyes and teeth', () => {
  it('shows the error and offers the search again', async () => {
    invoke.mockRejectedValue({ code: 'needs_original', message: 'The original is offline.' });
    const adjustments = { ...INITIAL_ADJUSTMENTS };
    const faceRetouch = {
      settings: {},
      faces: [{ id: 'face-1', kept: [], settings: {}, blemishes: [], box: [1, 0, 10, 10], enabled: true }],
      frame: '2:{}',
      geometry: JSON.stringify(faceRetouchGeometry(adjustments))
    } as unknown as FaceRetouch;
    useEditorStore.setState({
      selectedImage: { path: '/s/d.CR3', isReady: true } as never,
      adjustments: { ...adjustments, faceRetouch }
    });
    render(<StudioFaceRetouch onInteraction={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Find eyes and teeth' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    const again = await screen.findByRole('button', { name: 'Find eyes and teeth' });
    fireEvent.click(again);
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
  });
});

describe('the eye and teeth parser', () => {
  const eye = {
    iris: [5, 5, 2],
    lid: [
      [0, 0],
      [9, 0],
      [9, 9]
    ]
  };
  const loop = [
    [
      [0, 0],
      [4, 0],
      [4, 4]
    ]
  ];
  const analysed = {
    id: 'face-1',
    box: [0, 0, 10, 10],
    eyes: [eye, null],
    mouth: null,
    settings: {},
    kept: [],
    blemishes: []
  };
  const parsed = {
    ...analysed,
    eyes: [{ ...eye, parsedEye: loop, parsedIris: loop }, null],
    parsedTeeth: loop
  };

  it('leaves the skin sliders free, downloads only on the click and stores its outlines with the first value', async () => {
    let fail = true;
    invoke.mockImplementation((_command: string, args: { eyesTeeth?: boolean; downloadEyesTeeth?: boolean }) => {
      if (!args.eyesTeeth) return Promise.resolve({ frame: '2:{}', faces: [analysed] });
      if (!args.downloadEyesTeeth) return Promise.resolve({ frame: '2:{}', faces: [analysed], eyesTeethMissing: true });
      if (fail) {
        fail = false;
        return Promise.reject({ code: 'network', message: 'offline' });
      }
      return Promise.resolve({ frame: '2:{}', faces: [parsed] });
    });
    useEditorStore.setState({
      selectedImage: { path: '/s/e.CR3', isReady: true } as never,
      adjustments: { ...INITIAL_ADJUSTMENTS, faceRetouch: undefined }
    });
    render(<StudioFaceRetouch onInteraction={() => {}} />);
    const button = await screen.findByRole('button', { name: 'Download eye and teeth model' });
    expect((screen.getByRole('slider', { name: 'Face blemishes' }) as HTMLInputElement).disabled).toBe(false);
    expect((screen.getByRole('slider', { name: 'Iris (Both)' }) as HTMLInputElement).disabled).toBe(true);
    expect(invoke.mock.calls.some(([, a]) => (a as { downloadEyesTeeth?: boolean }).downloadEyesTeeth)).toBe(false);

    // A failed download offers the button again.
    fireEvent.click(button);
    fireEvent.click(await screen.findByRole('button', { name: 'Download eye and teeth model' }));
    await waitFor(() =>
      expect((screen.getByRole('slider', { name: 'Iris (Both)' }) as HTMLInputElement).disabled).toBe(false)
    );
    expect(invoke.mock.calls.filter(([, a]) => (a as { downloadEyesTeeth?: boolean }).downloadEyesTeeth)).toHaveLength(
      2
    );
    fireEvent.change(screen.getByRole('slider', { name: 'Iris (Both)' }), { target: { value: '40' } });
    await waitFor(() => expect(useEditorStore.getState().adjustments.faceRetouch?.faces[0].parsedTeeth).toEqual(loop));
    expect(useEditorStore.getState().adjustments.faceRetouch?.settings.iris).toBe(40);
  });

  it('keeps Face Mesh outlines for an edit whose Eyes were set on them', async () => {
    invoke.mockResolvedValue({ frame: '2:{}', faces: [parsed] });
    const adjustments = { ...INITIAL_ADJUSTMENTS };
    const faceRetouch = {
      settings: { iris: 50 },
      faces: [{ ...analysed, enabled: true }],
      frame: '2:{}',
      geometry: JSON.stringify(faceRetouchGeometry(adjustments))
    } as unknown as FaceRetouch;
    useEditorStore.setState({
      selectedImage: { path: '/s/f.CR3', isReady: true } as never,
      adjustments: { ...adjustments, faceRetouch }
    });
    render(<StudioFaceRetouch onInteraction={() => {}} />);
    fireEvent.change(screen.getByRole('slider', { name: 'Iris (Both)' }), { target: { value: '60' } });
    await waitFor(() => expect(useEditorStore.getState().adjustments.faceRetouch?.settings.iris).toBe(60));
    expect(invoke).not.toHaveBeenCalled();
    expect(useEditorStore.getState().adjustments.faceRetouch?.faces[0].parsedTeeth).toBeUndefined();
  });
});
