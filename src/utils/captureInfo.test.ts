import { describe, expect, it } from 'vitest';
import {
  captureInfoParts,
  formatAperture,
  formatFocalLength,
  formatIso,
  formatShutter,
  parseExposureTime,
} from './captureInfo';

describe('capture info under the photograph', () => {
  it('formats shutter speeds as fractions, then seconds', () => {
    expect(formatShutter(parseExposureTime('1/850 s'))).toBe('1/850 s');
    expect(formatShutter(parseExposureTime('1/3 s'))).toBe('1/3 s');
    expect(formatShutter(parseExposureTime('10/40'))).toBe('1/4 s');
    expect(formatShutter(parseExposureTime('1/2 s'))).toBe('0.5 s');
    expect(formatShutter(parseExposureTime('0.5 s'))).toBe('0.5 s');
    expect(formatShutter(parseExposureTime('1.3 s'))).toBe('1.3 s');
    expect(formatShutter(parseExposureTime('2 s'))).toBe('2 s');
    expect(formatShutter(parseExposureTime('30'))).toBe('30 s');
    expect(formatShutter(parseExposureTime('0.00125'))).toBe('1/800 s');
    expect(formatShutter(parseExposureTime(''))).toBeNull();
    expect(formatShutter(parseExposureTime('0 s'))).toBeNull();
  });

  it('formats aperture, focal length and ISO', () => {
    expect(formatAperture('f/2.8')).toBe('f/2.8');
    expect(formatAperture('f/2.799999952316284')).toBe('f/2.8');
    expect(formatAperture('8')).toBe('f/8');
    expect(formatAperture('f/11.0')).toBe('f/11');
    expect(formatFocalLength('35')).toBe('35 mm');
    expect(formatFocalLength('4.25')).toBe('4.3 mm');
    expect(formatFocalLength('50.0 mm')).toBe('50 mm');
    expect(formatIso('100')).toBe('ISO 100');
    expect(formatIso('ISO 6400')).toBe('ISO 6400');
    expect(formatAperture(undefined)).toBeNull();
  });

  it('shows ISO, shutter, aperture and focal length in that order, leaving out missing ones', () => {
    expect(
      captureInfoParts({ PhotographicSensitivity: '100', ExposureTime: '1/850 s', FNumber: 'f/2.8', FocalLength: '35' }),
    ).toEqual(['ISO 100', '1/850 s', 'f/2.8', '35 mm']);
    expect(captureInfoParts({ ISOSpeedRatings: '200', FocalLength: '23.5' })).toEqual(['ISO 200', '23.5 mm']);
    expect(captureInfoParts({})).toEqual([]);
    expect(captureInfoParts(null)).toEqual([]);
  });
});
