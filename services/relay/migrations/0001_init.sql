-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Relay state (ADR 0017). Nothing here names a person: submissions are keyed
-- by the SHA-256 of their id, rate limits by a keyed hash that changes with
-- each window, statistics are counters only.

-- One row per stored report, crash, camera test or RAW sample.
CREATE TABLE submissions (
  id_hash TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  ref TEXT NOT NULL,
  -- 'pending' (RAW sample announced, not uploaded yet) or 'filed'.
  state TEXT NOT NULL,
  issue_number INTEGER,
  issue_node_id TEXT,
  -- JSON array of R2 keys.
  objects TEXT NOT NULL DEFAULT '[]',
  received_at INTEGER NOT NULL
);
CREATE INDEX submissions_by_age ON submissions (kind, received_at);

-- One-time RAW sample upload tokens.
CREATE TABLE uploads (
  token_hash TEXT PRIMARY KEY,
  id_hash TEXT NOT NULL,
  object_key TEXT NOT NULL,
  size INTEGER NOT NULL,
  sha256 TEXT NOT NULL,
  -- JSON of the announced sample (camera, file type, note) for the issue.
  meta TEXT NOT NULL,
  expires INTEGER NOT NULL
);

-- Rate-limit and global-cap counters, deleted when their window ends.
CREATE TABLE rate (
  key TEXT PRIMARY KEY,
  count INTEGER NOT NULL,
  expires INTEGER NOT NULL
);

-- Proof-of-work stamps already used.
CREATE TABLE pow_seen (
  stamp TEXT PRIMARY KEY,
  expires INTEGER NOT NULL
);

-- Weekly usage statistics: counters only, never a send's body.
CREATE TABLE stats (
  week TEXT NOT NULL,
  dimension TEXT NOT NULL,
  value TEXT NOT NULL,
  count INTEGER NOT NULL,
  PRIMARY KEY (week, dimension, value)
);
-- Weeks whose rare values were folded into '(other)'.
CREATE TABLE stats_closed (
  week TEXT PRIMARY KEY
);
