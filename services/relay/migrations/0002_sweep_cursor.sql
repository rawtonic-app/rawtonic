-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Where the scheduled run's walk through the bucket stopped (ADR 0017): it
-- removes files no submission row names any more (a put that completed
-- after its report was deleted).
CREATE TABLE sweep_state (
  name TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
