# Where settings come from

Three sources, highest first:

```
environment          always wins — a one-off override, and what k8s injects
  ↓
AWS Secrets Manager  Huntwell_Local | Huntwell_Production
  ↓
the global file      the bootstrap credentials, and everything on a dev box
```

Secrets Manager sits above the file because the file holds the credentials that
opened it. If a value is in both, the remote one is the deliberate, rotatable
copy.

A box with no AWS credentials skips the middle step entirely — that is a
development box, and `local-infra/global` is the whole story there.

---

## The secret

One per environment. The name is chosen by `HUNTWELL_ENV`:

| `HUNTWELL_ENV` | secret read         |
|------------------|---------------------|
| `production`     | `Huntwell_Production` |
| anything else    | `Huntwell_Local`    |

`HUNTWELL_SECRET_ID` names one outright and skips that. The default leans
local on purpose: guessing wrong towards production means a dev box reading
live credentials.

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
  would point production at its own pod.

`POOL_PG_HOST` builds `HUNTWELL_POOL_DATABASE_URL` — the database *as the
worker pods reach it*, for when that address differs from the admin's. It
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
  "HUNTWELL_S3_SECRET_KEY":  "..."
}
```

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

Only the bootstrap credentials. Everything else belongs in Secrets Manager,
where it rotates without touching the host or rebuilding an image.

```
AWS_ACCESS_KEY_ID=AKIA...
AWS_SECRET_ACCESS_KEY=...
AWS_REGION=us-east-1
```

`HUNTWELL_AWS_ACCESS_KEY_ID` and friends are accepted too, and
`AWS_SESSION_TOKEN` for temporary credentials. On a machine with an instance
role, leave the file out and let the environment supply them.

Only a fixed list of names is read from this file — `HUNTWELL_*`,
`CURSOR_API_KEY`, `BROWSERBASE_*`, `STRIPE_*`, `COGNITO_*`, those four `AWS_*`
and the per-service `AWS_*_KEY` / `AWS_*_SECRET` pairs. A line
setting anything else is ignored, so a file in a working directory cannot set
`PATH`. A setting that seems not to apply is worth checking against that list
first.

`HUNTWELL_GLOBAL` names the file; otherwise it is `local-infra/global` in a
checkout, or `global` beside the binary.

### Sealed, in production

On a production box `global` is encrypted — AES-256-GCM, the same format and
tooling as Park River's `genesis`. The key lives in a **separate** file so the
config and the thing that opens it are never the same artifact: copying
`global` off the machine gets you nothing without `genesis`.

```bash
huntwell genesis keygen                      # once, off the server
huntwell genesis seal-file --out global.enc  # seal the whole file
huntwell genesis seal AWS_SECRET_ACCESS_KEY  # or one value, from stdin
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
