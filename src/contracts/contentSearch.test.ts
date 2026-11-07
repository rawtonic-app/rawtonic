// These payloads are also deserialized by Rust (`content_search::contract_tests`),
// which serializes its answers and events to the shapes listed here.
import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import payloads from './contentSearch.json';
import {
  contentSearchStatus,
  downloadContentSearch,
  rankByContent,
  startContentIndex,
  stopContentIndex,
  type ContentIndexProgress,
  type ContentRankingAnswer,
  type ContentSearchStatus
} from '../utils/contentSearch';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));

beforeEach(() => vi.mocked(invoke).mockClear());

const sent = () => vi.mocked(invoke).mock.calls[0];

it.each([
  ['start_content_index', () => startContentIndex('/Shoots/Job', true), payloads.start_content_index],
  ['start_content_index', () => startContentIndex('/Shoots/Job', false), payloads.start_content_index_periodic],
  ['stop_content_index', () => stopContentIndex('/Shoots/Job'), payloads.stop_content_index],
  ['stop_content_index', () => stopContentIndex(null), payloads.stop_content_index_any],
  ['content_search_status', () => contentSearchStatus('/Shoots/Job'), payloads.content_search_status],
  ['content_search_download', () => downloadContentSearch('/Shoots/Job'), payloads.content_search_download],
  [
    'content_search_rank',
    () => rankByContent('/Shoots/Job', 'a heron', '/Shoots/Job/Capture'),
    payloads.content_search_rank
  ],
  ['content_search_rank', () => rankByContent('/Shoots/Job', 'Hochzeit', null), payloads.content_search_rank_album]
])('%s sends the pinned payload', async (command, call, payload) => {
  await call();
  expect(sent()).toEqual([command, payload]);
});

it('the answers and events have the fields the frontend reads', () => {
  const status: ContentSearchStatus = payloads.status_result;
  const ranking: ContentRankingAnswer = payloads.rank_result;
  const progress: ContentIndexProgress = payloads.progress_event;
  const done: ContentIndexProgress = payloads.done_event;
  expect(Object.keys(status).sort()).toEqual(['downloadBytes', 'failed', 'indexed', 'installed', 'running', 'total']);
  expect(ranking.paths).toHaveLength(2);
  expect(progress.cancelled).toBeUndefined();
  expect(done.cancelled).toBe(false);
});
