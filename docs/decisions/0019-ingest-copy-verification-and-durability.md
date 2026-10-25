# ADR 0019: Ingest copies: one read, byte comparison, one drive flush per photograph

- Status: Accepted (task IM, G41, 2026-10-25)

## Context

An ingest of 200 RAW+JPEG pairs with a backup copy (6.6 GB, the UT2 card)
took 27–30 s, Lightroom Classic 11.5 s. A profile of the copy path
(`crates/ingest/examples/ingest_card.rs`) showed where the time went: SHA-256
50 % (every copy hashed the source while copying, then the copy and the source
again, for each destination: six hash passes over every byte), `F_FULLFSYNC`
25 % (on macOS `File::sync_all` flushes the drive cache; the copy path called it
for every file and every folder, about ten times per photograph), reads 14 %,
writes 7 %, the catalog under 2 %.

## Decision

Verification stays as strict as before, but is done once per source:

- A source of up to 256 MiB is read once into memory. Its SHA-256 (recorded
  with the photograph) comes from that buffer; every copy is read back and
  compared with it byte for byte, and so is the source, read again (a change
  while copying, even with its date kept). Larger sources stream: each chunk
  goes to every copy and the hash; the copies and the source are hashed again
  and compared. The library copy and the backup come from the same read, the
  companion JPEG's two copies likewise. Nothing is published before every copy
  and the source passed; publication stays no-clobber.
- While one photograph is written and checked, the next files are read and
  hashed ahead (at most 384 MiB not yet taken, files up to 128 MiB). This
  only reads; a file read ahead keeps the handle and the metadata of that
  read, so a change before its copy fails the same checks.

Durability: before, every file and folder was flushed to permanent storage
(`F_FULLFSYNC`) as it was written. Now files and folders are synced to the
drive as they are written (`fsync`), and the drive cache of each volume
written to is flushed once (`F_FULLFSYNC` on a folder of that volume) per
photograph, after its copies, companion, card sidecars and folders and before
the photograph is recorded in the library. Copies an earlier, cut-off run made
and this run reuses (the library copy, the backup, a companion, a sidecar;
`record_earlier_copy` of the hot folder) are synced again before that flush,
since that run's flush may not have happened. A drive flush writes the whole drive cache, so at
the moment the photograph is recorded, its copies, the backup and their folder
entries are on permanent storage as they were with the per-file flushes. What
a power cut can lose is unchanged: copies not yet recorded, which the ingest
job's write-ahead record (ADR 0013) finds and records on the next run. When the
flush fails, the photograph is not recorded; its copies stay for the next run
(tests `copies_not_made_durable_are_recorded_by_the_next_run`,
`copies_reused_by_the_next_run_are_flushed_again`). Copies of one file to
several places that fail together are made again one by one, so a failing
backup drive never keeps a companion JPEG out of the library
(`a_failed_backup_of_a_jpeg_leaves_its_library_copy`). On Linux and
Windows `sync_all` already reaches permanent storage, so nothing changes
there. `verified_copy` (moves, transfers) keeps its contract: durable when it
returns.

## Consequences

- The same card imports in 9.5–9.7 s from the command line (was 29.9 s), about
  the floor of that setup: reading the card image while writing two copies to
  the same SSD takes 9–10 s even without any check (G41 / BENCHMARKS).
- Peak memory of an ingest grows by up to the read-ahead budget plus the file
  being copied (≤ 384 MiB + 256 MiB; about 100 MiB for 30 MB RAWs).
- Re-reads of a fresh copy usually come from the page cache, as before; they
  check what the system will write, not the medium after the write.
