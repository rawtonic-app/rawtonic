# Windows WAL shared-handle lock transition

Origin: libsqlite3-sys 0.38.2 from crates.io (archive SHA-256
f1d20bef17f513b9b3004532233187769cd072d790971f4e4da0e346eb6401e8),
with its bundled SQLite 3.53.2. Original MIT license and SQLite public-domain
notices remain. Rawtonic's change is AGPL-3.0-or-later.

Only `sqlite3/sqlite3.c::winShmLock` changes executable behavior. On Windows,
SQLite uses one shared OS handle for paths beginning with two backslashes,
including verbatim local paths. The original code protects traversal of the
connection list, but releases the node mutex before changing the OS lock and
publishing the corresponding connection mask. Two readers can consequently
both acquire a shared OS lock, or race the last unlock against another reader.
The masks no longer describe the locks and subsequent reads can exhaust
SQLite's own WAL retry bound with SQLITE_PROTOCOL.

The patch holds that existing mutex across the entire shared-handle transition.
The per-connection-handle branch is unchanged. Local sibling contention returns
BUSY before an OS wait, so the mutex does not prevent a sibling from releasing
an OS lock awaited by this call. No path, WAL, durability, timeout or retry
semantics change. Connections remain owned by their respective threads.

Primary references:
- https://github.com/sqlite/sqlite/blob/master/src/os_win.c (same split critical
  sections when inspected 2026-10-29; no upstream correction found).
- https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-lockfileex
  (overlapping shared locks on one handle and matching unlock operations).

The original catalog regression uses four threads and 1500 synchronized
open/read/close rounds. A standalone version using only rusqlite reproduced
SQLITE_PROTOCOL with verbatim paths while ordinary paths passed. Windows
validation at 9d461878 passed all three concurrency tests in 27.13 seconds:
the original catalog regression, migration from DELETE to WAL, and a second
four-thread stress test using a long verbatim path with a literal trailing-dot/
space directory and an integrity check. This is local Windows VFS evidence,
not a network-share qualification.
Remove this vendored patch when the bundled upstream version corrects the
whole transition and the regression passes without it. The optional SQLCipher
amalgamation is unchanged; Rawtonic uses bundled SQLite, not SQLCipher.
