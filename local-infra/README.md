# local-infra

One isolated Huntwell infrastructure per `INSTANCE`, entirely inside this
directory: a Postgres cluster, its logs and pid files, the Chrome profiles and
agent workspaces the runner uses, and the `global` env file the server reads.

```
./local-infra/start.sh                # default instance
INSTANCE=pra ./local-infra/start.sh   # a second one — different ports, data, env
FRESH=1 ./local-infra/start.sh        # wipe this instance's data first
./local-infra/stop.sh                 # stop (INSTANCE=pra ./local-infra/stop.sh)
```

```
local-infra/
  start.sh, stop.sh          bring the cluster up / down
  config                     INSTANCE, port bases, PG_BIN (see config.example; gitignored)
  global                     what the server reads: DB URL, ports, CURSOR_API_KEY … (gitignored)
  db/public/<table>.sql      the schema, one file per table, snake_case names
  db/schema.order            the order they are applied in
  db/data/public.<table>.csv optional seed rows, loaded only into empty tables
  data/                      the default instance (gitignored)
    postgres/                PGDATA
    huntwell/              Chrome profiles + agent workspaces, per account
    logs/                    initdb, postgres, db-init
    temp/                    pid files (postgres, minio, nats, plus ./dev.sh's
                             admin/server/vite/dev), unix socket
  data-<instance>/           the same for a named instance
```

Ports are `base + offset`; the offset is a stable hash of the instance name so
two instances never collide. `start.sh` prints the block it chose.

Things worth knowing:

- **The database persists between starts.** Unlike a throwaway dev DB, this
  holds accounts and scraped prospects; `FRESH=1` is the only thing that
  deletes it.
- **Schema changes are SQL files here, nothing else.** The server applies
  `db/public/*.sql` in `schema.order` at startup, and so does `start.sh`, so
  a `CREATE TABLE` in Rust would be a second source of truth. Files must stay
  idempotent (`IF NOT EXISTS`); a change to an existing table is an
  `ALTER TABLE … ADD COLUMN IF NOT EXISTS` appended to that table's file.
- **`global` is rewritten by `start.sh`** but only the lines it owns; your
  `CURSOR_API_KEY` and everything below the marker survive.
