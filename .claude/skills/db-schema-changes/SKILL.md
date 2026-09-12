---
name: db-schema-changes
description: Read before adding or altering any table, column, index or constraint, before writing CREATE TABLE / ALTER TABLE anywhere, and before touching store.rs queries that assume a column exists.
---

# Schema changes in Huntwell

The schema is the set of SQL files in `local-infra/db/public/`, applied in the
order listed in `local-infra/db/schema.order`. Two things apply them and both
must agree: `local-infra/start.sh` (via `psql`) and the server at startup
(`store::migrate`, over the string `cmd/build.rs` concatenates). Nothing else
creates or alters a table.

## Rules

1. **One file per table**, named after the table, Postgres-standard snake_case
   identifiers (`plan_id`, `created_at`), singular table names.
2. **Idempotent.** `CREATE TABLE IF NOT EXISTS`, `CREATE INDEX IF NOT EXISTS`.
   A new column on an existing table is appended to that table's file as
   `ALTER TABLE public.t ADD COLUMN IF NOT EXISTS col type NOT NULL DEFAULT …;`
   — never edited into the original `CREATE TABLE`, which has already run on
   every existing cluster.
3. **New table** → new file + a line in `schema.order` *after* every table it
   references.
4. **Tenant tables carry `account_id`** and every store function filters on it.
   Child tables key on `plan_id` with `ON DELETE CASCADE`.
5. Comment the *why* above each non-obvious column, as the existing files do.
6. After the change: `./local-infra/start.sh` (applies it), `cd cmd && cargo build`
   (re-embeds it), then update `store.rs` and the TS types in `UI/web/src/api.ts`.

## Banned

- `CREATE TABLE` / `ALTER TABLE` in Rust or TypeScript.
- Editing a shipped `CREATE TABLE` body to add a column.
- Hand-running SQL against a shared cluster instead of committing the file.
