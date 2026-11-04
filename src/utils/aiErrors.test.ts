import '../i18n';
import i18n from 'i18next';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const { toast } = vi.hoisted(() => ({ toast: { error: vi.fn() } }));
vi.mock('../activity/notify', () => ({ notify: toast }));

import {
  aiErrorMessage,
  isAiRuntimeError,
  isAiTaskCancelled,
  isPhotoChanged,
  noteBackgroundAiError,
  resetAiErrorNotices,
  toastAiError
} from './aiErrors';

/** The backend's error for a broken runtime (rawtonic_onnx::RuntimeError). */
const runtimeError = {
  code: 'ai_runtime_unavailable',
  message: 'AI runtime unavailable: /app/resources/libonnxruntime.dylib could not be loaded: dlopen failed'
};
const cancelled = { code: 'cancelled', message: 'Task cancelled' };

beforeEach(() => {
  toast.error.mockReset();
  resetAiErrorNotices();
  void i18n.changeLanguage('en');
});

describe('AI errors', () => {
  it('recognises the runtime error and shows translated text for it', () => {
    expect(isAiRuntimeError(runtimeError)).toBe(true);
    expect(isAiRuntimeError(cancelled)).toBe(false);
    // The code decides, not the text.
    expect(isAiRuntimeError(runtimeError.message)).toBe(false);
    expect(isAiRuntimeError({ code: 'internal', message: runtimeError.message })).toBe(false);
    expect(aiErrorMessage(runtimeError)).toMatch(/reinstall Rawtonic.*report a problem/i);
    void i18n.changeLanguage('de');
    expect(aiErrorMessage(runtimeError)).toMatch(/^Die KI-Laufzeitumgebung/);
    // Other errors keep the tool's own text.
    expect(aiErrorMessage({ code: 'models_missing', message: 'face-models-missing' })).toBe('face-models-missing');
    expect(aiErrorMessage('boom')).toBe('boom');
    expect(aiErrorMessage('boom', 'Sky failed: boom')).toBe('Sky failed: boom');
  });

  it('tells a cancelled task and a changed photo by their codes', () => {
    expect(isAiTaskCancelled(cancelled)).toBe(true);
    expect(isAiTaskCancelled({ code: 'internal', message: 'Load cancelled' })).toBe(false);
    expect(isAiTaskCancelled('Task cancelled')).toBe(false);
    const changed = { code: 'stale', message: 'The photo changed while its mask was asked for' };
    expect(isPhotoChanged(changed)).toBe(true);
    expect(isPhotoChanged(changed.message)).toBe(false);
    expect(isPhotoChanged(cancelled)).toBe(false);
  });

  it('shows one runtime toast for several failing tools, and none for a cancelled task', () => {
    toastAiError(cancelled, 'AI Mask Failed: Task cancelled');
    expect(toast.error).not.toHaveBeenCalled();
    toastAiError(runtimeError, `AI Sky Mask Failed: ${runtimeError.message}`);
    toastAiError(runtimeError, `AI Foreground Mask Failed: ${runtimeError.message}`);
    expect(toast.error).toHaveBeenCalledTimes(2);
    for (const [text, options] of toast.error.mock.calls) {
      expect(text).toMatch(/^The AI runtime could not be loaded/);
      // The same id: react-toastify keeps a single toast.
      expect(options).toEqual({ toastId: 'ai-runtime-unavailable', source: 'ai' });
    }
    toastAiError('out of memory', 'AI Sky Mask Failed: out of memory');
    expect(toast.error).toHaveBeenLastCalledWith('AI Sky Mask Failed: out of memory', { source: 'ai' });
  });

  it('reports a missing runtime for background work once per session, and nothing else', () => {
    noteBackgroundAiError('Failed to read directory');
    expect(toast.error).not.toHaveBeenCalled();
    noteBackgroundAiError(runtimeError);
    noteBackgroundAiError(runtimeError);
    expect(toast.error).toHaveBeenCalledTimes(1);
  });
});
