import '../i18n';
import i18n from 'i18next';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const { toast } = vi.hoisted(() => ({ toast: { error: vi.fn() } }));
vi.mock('react-toastify', () => ({ toast }));

import {
  aiErrorMessage,
  isAiRuntimeError,
  noteBackgroundAiError,
  resetAiErrorNotices,
  toastAiError,
} from './aiErrors';

/** The backend's error for a broken runtime (rawtonic_onnx::RuntimeError). */
const runtimeError =
  'AI runtime unavailable: /app/resources/libonnxruntime.dylib could not be loaded: dlopen failed';

beforeEach(() => {
  toast.error.mockReset();
  resetAiErrorNotices();
  void i18n.changeLanguage('en');
});

describe('AI errors', () => {
  it('recognises the runtime error and shows translated text for it', () => {
    expect(isAiRuntimeError(runtimeError)).toBe(true);
    expect(isAiRuntimeError(new Error(runtimeError))).toBe(true);
    expect(isAiRuntimeError('Task cancelled')).toBe(false);
    expect(aiErrorMessage(runtimeError)).toBe(
      'The AI runtime could not be loaded, so AI tools are unavailable. Everything else works as usual.',
    );
    void i18n.changeLanguage('de');
    expect(aiErrorMessage(runtimeError)).toMatch(/^Die KI-Laufzeitumgebung/);
    // Other errors keep the tool's own text.
    expect(aiErrorMessage('face-models-missing')).toBe('face-models-missing');
    expect(aiErrorMessage('boom', 'Sky failed: boom')).toBe('Sky failed: boom');
  });

  it('shows one runtime toast for several failing tools, and none for a cancelled task', () => {
    toastAiError('Task cancelled', 'AI Mask Failed: Task cancelled');
    expect(toast.error).not.toHaveBeenCalled();
    toastAiError(runtimeError, `AI Sky Mask Failed: ${runtimeError}`);
    toastAiError(runtimeError, `AI Foreground Mask Failed: ${runtimeError}`);
    expect(toast.error).toHaveBeenCalledTimes(2);
    for (const [text, options] of toast.error.mock.calls) {
      expect(text).toMatch(/^The AI runtime could not be loaded/);
      // The same id: react-toastify keeps a single toast.
      expect(options).toEqual({ toastId: 'ai-runtime-unavailable' });
    }
    toastAiError('out of memory', 'AI Sky Mask Failed: out of memory');
    expect(toast.error).toHaveBeenLastCalledWith('AI Sky Mask Failed: out of memory');
  });

  it('reports a missing runtime for background work once per session, and nothing else', () => {
    noteBackgroundAiError('Failed to read directory');
    expect(toast.error).not.toHaveBeenCalled();
    noteBackgroundAiError(runtimeError);
    noteBackgroundAiError(runtimeError);
    expect(toast.error).toHaveBeenCalledTimes(1);
  });
});
