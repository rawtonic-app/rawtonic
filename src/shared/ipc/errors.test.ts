import { describe, expect, it } from 'vitest';
import { errorCode, errorMessage, isCommandError } from './errors';

describe('command errors', () => {
  it('reads code and message of a converted command', () => {
    const error = { code: 'cancelled', message: 'Task cancelled' };
    expect(isCommandError(error)).toBe(true);
    expect(errorCode(error)).toBe('cancelled');
    expect(errorMessage(error)).toBe('Task cancelled');
  });

  it('keeps a plain string error as its message, without a code', () => {
    expect(errorCode('cancelled')).toBeUndefined();
    expect(errorMessage('cancelled')).toBe('cancelled');
  });

  it('shows other rejections as before', () => {
    expect(errorCode(new Error('boom'))).toBeUndefined();
    expect(errorMessage(new Error('boom'))).toBe('boom');
    expect(errorMessage(undefined)).toBe('undefined');
    expect(errorMessage({ message: 'no code' })).toBe('no code');
    expect(errorCode({ message: 'no code' })).toBeUndefined();
    expect(errorMessage({ other: 1 })).toBe('[object Object]');
    expect(errorCode(null)).toBeUndefined();
  });

  it('keeps the extra fields of a structured failure readable', () => {
    const error = { code: 'internal', message: 'Could not save', failed: ['a.raf'] };
    expect(errorMessage(error)).toBe('Could not save');
    expect((error as { failed: string[] }).failed).toEqual(['a.raf']);
  });
});
