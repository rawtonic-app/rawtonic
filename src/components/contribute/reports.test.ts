import { describe, expect, it } from 'vitest';
import {
  MAX_URL,
  crashPayload,
  githubIssue,
  photoInfo,
  plainName,
  reportMarkdown,
  reportPayload,
  type CrashRecord,
  type ReportContext,
} from './reports';

const context: ReportContext = {
  app: { version: '0.1.0-alpha.4', os: 'macos', osVersion: '26.5', arch: 'aarch64' },
  gpu: { vendor: 'apple', name: 'Apple M5 Pro' },
  logTail: 'line 1\nline 2',
  screenshotAvailable: true,
};

const record: CrashRecord = {
  id: '1792800000-00000000000000ab',
  crashedOn: '2026-10-23',
  created: 1792800000,
  app: context.app,
  gpu: { vendor: 'apple' },
  message: 'index out of bounds',
  location: 'decode.rs:12:5',
  backtrace: '0: rawdecode::open (decode.rs:12:5)',
  logTail: 'opening <path>.CR3',
  asked: false,
};

describe('report payloads', () => {
  it('holds only what the user chose', () => {
    const shot = { mime: 'image/jpeg' as const, data: 'AAAA', width: 2, height: 1 };
    expect(
      reportPayload({ text: ' It froze ', context, includeSystem: false, logTail: null, photo: null, screenshot: null }),
    ).toEqual({ schema: 1, app: context.app, text: 'It froze' });
    expect(
      reportPayload({
        text: 'It froze',
        context,
        includeSystem: true,
        logTail: 'edited',
        photo: { make: 'Nikon', model: 'Z 6', fileType: 'NEF' },
        screenshot: shot,
      }),
    ).toEqual({
      schema: 1,
      app: context.app,
      gpu: context.gpu,
      text: 'It froze',
      logTail: 'edited',
      photo: { make: 'Nikon', model: 'Z 6', fileType: 'NEF' },
      screenshot: { mime: 'image/jpeg', data: 'AAAA' },
    });
  });

  it('builds a crash payload with the backtrace and log removable', () => {
    expect(crashPayload(record, { includeBacktrace: false, logTail: null })).toEqual({
      schema: 1,
      app: record.app,
      gpu: record.gpu,
      message: 'index out of bounds',
      location: 'decode.rs:12:5',
      crashedOn: '2026-10-23',
    });
    const full = crashPayload(record, { includeBacktrace: true, logTail: 'mine', message: 'index out of <redacted>' });
    expect(full.message).toBe('index out of <redacted>');
    expect(full.backtrace).toBe(record.backtrace);
    expect(full.logTail).toBe('mine');
  });

  it('names the camera only with names the relay takes', () => {
    expect(plainName('NIKON CORPORATION\n')).toBe('NIKON CORPORATION');
    expect(plainName('Canon™ <EOS> R5')).toBe('Canon EOS R5');
    expect(plainName('™')).toBeNull();
    expect(photoInfo({ path: '/Users/anna/Pictures/DSC_1.nef', exif: { Make: 'NIKON', Model: 'Z 6' } })).toEqual({
      make: 'NIKON',
      model: 'Z 6',
      fileType: 'NEF',
    });
    expect(photoInfo({ path: 'C:\\p\\IMG_1.CR3?vc=2', exif: { Make: 'Canon', Model: 'EOS R5' } })?.fileType).toBe('CR3');
    expect(photoInfo({ path: '/x/no-extension', exif: { Make: 'Canon', Model: 'EOS R5' } })).toBeNull();
    expect(photoInfo({ path: '/x/a.jpg', exif: null })).toBeNull();
    expect(photoInfo(null)).toBeNull();
  });
});

describe('the no-relay fallbacks', () => {
  it('copies the prepared body as text, with fences no content can close', () => {
    const body = JSON.stringify({ schema: 1, app: context.app, text: 'Broken', logTail: 'a ``` b', screenshot: { mime: 'image/jpeg', data: 'AAAA' } });
    const text = reportMarkdown(body);
    expect(text).toContain('Broken');
    expect(text).toContain('Rawtonic 0.1.0-alpha.4 · macos 26.5 (aarch64)');
    expect(text).toContain('````\na ``` b\n````');
    expect(text).not.toContain('AAAA');
    const crash = reportMarkdown(JSON.stringify(crashPayload(record, { includeBacktrace: true, logTail: 'x' })));
    expect(crash).toContain('Rawtonic quit unexpectedly on 2026-10-23 at `decode.rs:12:5`.');
    expect(crash).toContain('rawdecode::open');
  });

  it('fits GitHub links within the limit, dropping the oldest log lines first', () => {
    const log = Array.from({ length: 400 }, (_, n) => `log line ${n} ${'x'.repeat(40)}`).join('\n');
    const body = JSON.stringify({ schema: 1, app: context.app, text: 'Export stops', logTail: log });
    const { url, body: shownBody } = githubIssue(body);
    expect(url.length).toBeLessThanOrEqual(MAX_URL);
    expect(url.startsWith('https://github.com/rawtonic-app/rawtonic/issues/new?title=Problem%3A%20Export%20stops&body=')).toBe(true);
    const shown = new URL(url).searchParams.get('body') ?? '';
    expect(shown).toContain('Export stops');
    expect(shown).toContain('log line 399');
    expect(shown).not.toContain('log line 0 ');
    expect(shown).toContain('Shortened to fit a link');
    // What the review shows is what the link carries.
    expect(shown).toBe(shownBody);
    // A short report is not shortened.
    const short = new URL(githubIssue(JSON.stringify({ schema: 1, app: context.app, text: 'Hi' })).url).searchParams.get('body');
    expect(short).not.toContain('Shortened');
    // Even a text at the relay's cap fits.
    const long = githubIssue(JSON.stringify({ schema: 1, app: context.app, text: 'é'.repeat(10_000) })).url;
    expect(long.length).toBeLessThanOrEqual(MAX_URL);
    // A crash keeps its place, date and system when its message is shortened.
    const crash = githubIssue(
      JSON.stringify(crashPayload({ ...record, message: '異'.repeat(2_000) }, { includeBacktrace: true, logTail: 'x\ny' })),
    );
    expect(crash.url.length).toBeLessThanOrEqual(MAX_URL);
    expect(crash.body).toContain('at `decode.rs:12:5`');
    expect(crash.body).toContain('2026-10-23');
    expect(crash.body).toContain('Rawtonic 0.1.0-alpha.4');
    expect(crash.body).toContain('異異');
  });
});
