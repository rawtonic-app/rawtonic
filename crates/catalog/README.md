# rawtonic-catalog

The library: sessions and catalogs, their SQLite database (authoritative for
managed photographs), metadata, recipe history and the library search. See the
`catalog` section of `docs/ARCHITECTURE.md`.

## Schema migrations

`src/migrations.rs` holds every schema step in one table, `MIGRATIONS`, and the
one runner that applies them. A new library is migrated from schema 0; an
opened one from its `PRAGMA user_version`, all steps in one immediate
transaction, so a failing step leaves the library as it was. Databases newer
than the last step are refused.

Adding schema 25:

1. Write `migrations/0025.sql`, ending with `PRAGMA user_version=25;`. A step
   that shipped never changes: libraries already went through it.
2. Add one line to `MIGRATIONS`:
   `Migration::sql(25, include_str!("../migrations/0025.sql")),`
   (`Migration::with_columns` when the step adds columns that must be added only
   where missing). `DB_SCHEMA_VERSION` follows from the table.
3. Add a populated fixture for schema 25 and record what it upgrades to
   (`tests/migration_fixtures.rs` says how); the recorded results of the older
   fixtures change only where the new step changes data on purpose — review that
   diff.
