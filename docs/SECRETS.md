# Where settings come from

Nothing is configured through environment variables. The chain is Park River's:

```
genesis key          a file beside or above the executable; opens the sealed global
  ↓
global (sealed)      KEY and SECRET — the AWS credential, and nothing else
  ↓
Secrets Manager      the database connection and every credential
  ↓
setting table        everything else — set with `admin config set`
```

When a setting is read, the most specific source wins:

```
1. process environment   an emergency override — nothing needs it
2. setting table         admin config set NAME VALUE
3. Secrets Manager       Huntwell_Production | Huntwell_Local
4. global file           the bootstrap credential; everything on a dev box
```

The table sits above Secrets Manager because it is what an operator actually
edits; the environment stays on top only so a bad row can be overridden without
a database round trip. Anything read before the database is open sees an empty
table layer — which is why the connection string lives in Secrets Manager.

A box with no AWS credentials skips Secrets Manager entirely — that is a
development box, and `local-infra/global` is the whole story there.

### Operator settings

```bash
admin config set HUNTWELL_PUBLIC_URL https://app.yourdomain.com
admin config get HUNTWELL_PUBLIC_URL       # prints just the value
admin config list                          # names from each source
admin config set HUNTWELL_PUBLIC_URL ""    # clears it
```

The admin's own listen address is not a setting but a parameter, as in Park
River: `admin serve --addr 10.121.17.195:8710`.

A running admin picks a change up on restart; a VM picks it up on its next
**Deploy**, which rewrites its settings file.

**Never put a credential in the setting table.** Every process that can reach
the database can read it — including every worker VM — so a credential there
would undo giving each VM only its own role's secrets. `admin config set`
refuses credential-shaped names (`*_KEY`, `*_SECRET`, `*TOKEN*`, `*PASSWORD*`,
database URLs and parts) and says to use Secrets Manager.

### Inside a VM

The VMs are the one place a settings file exists, and it is written by the
admin, not by you — exactly as Park River writes `/parkriver/env`. At provision
and on every Deploy the admin resolves the chain above and writes the values
that VM's role needs into `/huntwell/env` (root, 0600). A worker VM's file holds
the database, Cursor and Browserbase; only the app VM's holds identity, mail and
the session secret.

---

## The secret

One per environment, chosen by the build — no variable to set:

| Build | Secret read |
|---|---|
| `./build.sh` (release) — what a server runs | `Huntwell_Production` |
| `./dev.sh` (debug) | `Huntwell_Local` |

`HUNTWELL_SECRET_ID` in the sealed global names one outright, for the rare case
that needs it.

The secret is a **JSON object of name to value**. Non-string values are read as
their JSON text, so a port typed as a number in the console still arrives.

---

## Database: parts, not a URL

Write the connection as separate fields:

```json
{
  "HUNTWELL_PG_HOST":     "db.internal",
  "HUNTWELL_PG_PORT":     "5432",
  "HUNTWELL_PG_USERNAME": "huntwell",
  "HUNTWELL_PG_PASSWORD": "...",
  "HUNTWELL_PG_DATABASE": "huntwell",
  "HUNTWELL_PG_SSLMODE":  "require"
}
```

They assemble into `HUNTWELL_DATABASE_URL`. The point is rotation: a managed
rotation replaces `..._PASSWORD` and the next fetch picks it up, with no URL
anywhere to rewrite.

- **An explicit `HUNTWELL_DATABASE_URL` in the secret always wins.**
  Assembling is the fallback, never an override.
- The password is percent-encoded into the URL. A generated password
  eventually contains an `@` or a `/`, and an unencoded one reparses the URL
  into a different host — an error naming somewhere nobody configured.
- `PG_*` and `DB_*` are accepted as well, which is what an RDS-managed secret
  calls them. `USER` for `USERNAME`, `DBNAME` for `DATABASE`.
- **No host means no URL**, rather than a wrong one. Defaulting to localhost
  would point a VM at itself.

`POOL_PG_HOST` builds `HUNTWELL_POOL_DATABASE_URL` — the database *as the
VMs reach it*, for when that address differs from the admin's. It
inherits any credential it does not restate, and is not built at all unless
that host is given.

---

## Everything else

Plain names, exactly as the application reads them:

```json
{
  "HUNTWELL_SESSION_SECRET": "...",
  "CURSOR_API_KEY":            "...",
  "BROWSERBASE_API_KEY":       "...",
  "BROWSERBASE_PROJECT_ID":    "...",
  "AWS_SES_KEY":             "...",
  "AWS_SES_SECRET":          "...",
  "HUNTWELL_MAIL_FROM":      "Huntwell <hello@yourdomain.com>",
  "HUNTWELL_S3_ACCESS_KEY":  "...",
  "HUNTWELL_S3_SECRET_KEY":  "...",
  "HUNTWELL_NATS_USER":      "huntwell",
  "HUNTWELL_NATS_PASSWORD":  "...",
  "HUNTWELL_NATS_TLS_CA":    "-----BEGIN CERTIFICATE-----\n...",
  "HUNTWELL_NATS_TLS_CERT":  "-----BEGIN CERTIFICATE-----\n...",
  "HUNTWELL_NATS_TLS_KEY":   "-----BEGIN PRIVATE KEY-----\n..."
}
```

The NATS certificates are made as in PRODUCTION.md ("The bus's TLS
certificates"). They reach VMs as files, never in a settings file.

---

## Bot check: Cloudflare Turnstile

Sign-up and sign-in require a Turnstile token once the secret is set. Make a
widget at Cloudflare › Turnstile for the site's hostname (managed mode is
fine) and put both keys in the secret — the site key is public, but its name
ends in `_KEY` so `admin config set` refuses it:

```json
{
  "TURNSTILE_SITE_KEY":   "0x4AAAA...",
  "TURNSTILE_SECRET_KEY": "0x4AAAA..."
}
```

With no secret nothing is checked and the forms show no widget, which is what
`./dev.sh` does. Each token is verified once with Cloudflare, bound to the
visitor's address, and a refused token is logged with Cloudflare's reason —
a wrong secret looks like every visitor failing.

---

## Identity

Passwords live in an AWS Cognito user pool, not in the database. The `account`
row keys to the pool by `cognito_sub` and stores no hash — the same arrangement
as Park River's `identity` module, which this is a port of.

```json
{
  "COGNITO_USER_POOL_ID":  "us-east-1_AbC123",
  "COGNITO_CLIENT_ID":     "...",
  "COGNITO_CLIENT_SECRET": "...",
  "COGNITO_REGION":        "us-east-1",
  "AWS_COGNITO_KEY":       "AKIA...",
  "AWS_COGNITO_SECRET":    "..."
}
```

- The app client needs **`ALLOW_USER_PASSWORD_AUTH`**. Sign-in is an unsigned
  `InitiateAuth` — the call a person's own password authorises — so the login
  path needs no AWS credential at all.
- `AWS_COGNITO_KEY`/`_SECRET` is an IAM user allowed `AdminCreateUser`,
  `AdminSetUserPassword` and `AdminDeleteUser` on that pool, and nothing else.
  Creating an account is something the platform does, so those calls are
  SigV4-signed; the bootstrap credential is deliberately not accepted for them.
- `COGNITO_CLIENT_SECRET` only if the app client has one. A client with a
  secret used without one is refused outright, with an error that blames the
  credentials.

**`HUNTWELL_IDENTITY`** overrides the choice: `local` keeps argon2 hashes in
the `account` row, `cognito` requires the pool. Unset, a configured pool means
Cognito and no pool means local — so `./dev.sh` signs people in with no AWS
credentials anywhere, and production does not.

The operator console (`admin_user`) is deliberately outside this. It is
bootstrapped by a setup key printed at first boot, not by the product's
directory, so losing access to the pool never locks you out of the control
plane.

---

## The global file

Only the bootstrap credential — the same two names as Park River's:

```
KEY=AKIA...
SECRET=...
```

The region defaults to `us-east-1`; add `AWS_SECRETS_REGION=` if the secret
lives elsewhere. `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` are accepted too,
and `AWS_SESSION_TOKEN` for temporary credentials. Everything else belongs in
Secrets Manager or the setting table. On a machine with an instance
role, leave the file out and let the environment supply them.

Only a fixed list of names is read from this file — `KEY`, `SECRET`,
`HUNTWELL_*`, `CURSOR_API_KEY`, `BROWSERBASE_*`, `STRIPE_*`, `COGNITO_*`, the
bootstrap `AWS_*` names and the per-service `AWS_*_KEY` / `AWS_*_SECRET` pairs. A line
setting anything else is ignored, so a file in a working directory cannot set
`PATH`. A setting that seems not to apply is worth checking against that list
first.

`HUNTWELL_GLOBAL` names the file; otherwise it is `local-infra/global` in a
checkout, or the nearest `global` beside or above the binary (`/yaksoft/bin/admin` reads `/yaksoft/global`).

### Encrypted, in production

Read exactly as Park River reads its `global`. The genesis key is **compiled
into the binary** by `build.rs` — `local-infra/genesis_prod` (or `.txt`) for a
release build, `genesis_local` for a debug one — so the server holds only the
encrypted file. `./build.sh` refuses to build without the key
(`HUNTWELL_GENESIS_DIR` points elsewhere; `--local` picks the local key).

Encrypt the file on the machine that holds the key, with openssl:

```bash
openssl enc -aes-256-cbc -pbkdf2 -iter 600000 -salt \
  -pass file:local-infra/genesis_prod.txt -in global.plain -out global
```

The first file called `global` at or above the working directory, then the
executable's folder, is the file. One that will not open is an error at
startup naming why — it is never skipped for another.

`huntwell genesis seal-file` writes the AES-256-GCM form instead, which opens
the same way and fails closed on tampering:

```bash
huntwell genesis seal-file --out global.enc  # seal the whole file
huntwell genesis seal SECRET                 # or one value, from stdin
huntwell genesis show                        # what this binary can see
```

A whole-file seal is one `enc:v1:` line, so not even the *names* of the
settings are readable. A per-value seal — `NAME=enc:v1:…` — leaves the rest of
the file plain, which is the shape to use when only one line is a credential.
Both are read transparently; nothing above `config` knows the difference.

Two properties worth relying on:

- **The setting's name is bound into the ciphertext.** Pasting the value of
  `AWS_SECRET_ACCESS_KEY` over `AWS_ACCESS_KEY_ID` fails to decrypt rather than
  quietly swapping two credentials.
- **A tampered value fails**, because GCM is authenticated — it does not decode
  to garbage the way `openssl enc` in CBC mode would.

A sealed value that will not open reads as *unset*, with the reason on stderr.
A sealed *file* that will not open is louder still: the loader says so and
skips the file rather than behaving as though nothing were configured.

Back the key up when you make it. Without it a sealed `global` cannot be
opened, and `keygen` refuses to overwrite an existing key for the same reason.

---

## What gets logged

At startup, names only:

```
secrets: 7 setting(s) from Huntwell_Production — CURSOR_API_KEY, HUNTWELL_DATABASE_URL, ...
```

Never values. That line gets pasted into issues.

A failure is logged at ERROR and the process continues on whatever the file and
environment provide — a missing secret should be loud, not fatal, because the
alternative is a service that will not start and cannot say why.
