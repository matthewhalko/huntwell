# Operations

## Local

```bash
./local-infra/start.sh                 # Postgres up, schema applied, global written
./dev.sh                               # server (--dev) + Vite
./dev.sh --no-ui                       # server only
cmd/target/debug/huntwell doctor     # agent / npx / Chrome / database check
./local-infra/stop.sh
FRESH=1 ./local-infra/start.sh         # wipe this instance's data
```

Logs: `local-infra/data/logs/*.log` (Postgres), the server's stdout, and per
run the `execution_log` table (visible live in the UI).

## Release

```bash
./build.sh            # zig cross-compile, linux/amd64 -> bin/ubuntu/
./build.sh arm64      # -> bin/ubuntu-arm64/
./build.sh host       # native
```

**Production is [PRODUCTION.md](PRODUCTION.md)**: Incus VMs on bare-metal
servers, driven by the admin. What follows is the single-server alternative —
one binary, runs as child processes — the simplest thing that works, with no
control plane to operate.

On the server:

1. Postgres 16+ with a database and a role; put `HUNTWELL_DATABASE_URL` in
   `/opt/huntwell/global` (copy `global.example`), plus `HUNTWELL_ADDR`,
   `HUNTWELL_DATA_DIR`, `HUNTWELL_SESSION_SECRET`, `CURSOR_API_KEY`.
2. Install the Cursor `agent` CLI, Node 20+, Google Chrome, and `xvfb`
   (`HUNTWELL_CHROME_DISPLAY=xvfb`, or `headless` if you accept the
   detectability trade-off).
3. Install `deploy/huntwell.service.in` with `@APP_DIR@`/`@APP_USER@`
   substituted; `systemctl enable --now huntwell`.
4. Put Caddy (or any TLS terminator) in front — `deploy/Caddyfile.example` —
   and set `HUNTWELL_TRUST_PROXY=1` (the *last* `X-Forwarded-For` entry, the
   one the proxy appends, is taken; `cloudflare` reads `CF-Connecting-IP`).
5. Sign up, then set `HUNTWELL_OPEN_SIGNUP=0` and restart.

To run executions in worker VMs instead of as child processes of this server,
use the admin control plane — see [PRODUCTION.md](PRODUCTION.md).

The binary applies the schema at startup (idempotent, under an advisory lock),
so a deploy is: copy binary, restart.

## Things that will bite

| Symptom | Cause |
|---|---|
| Runs fail immediately with "no browser to scrape with" | No display and no Xvfb; set `HUNTWELL_CHROME_DISPLAY` or install `xvfb` |
| Run log shows the agent guessing at `prospects` server names | `huntwell mcp-prospects` could not start — usually `HUNTWELL_DATABASE_URL` not reaching the child; check `huntwell config get HUNTWELL_DATABASE_URL` as the service user |
| "plan is already running" but nothing is | The previous server died mid-run; restart the server (it marks stale runs failed) or cancel from the UI |
| Everyone signed out after a restart | `HUNTWELL_SESSION_SECRET` missing — sessions are stored server-side, but cookies are not `Secure` in `--dev`; in production make sure the file is readable by the service user |
| API rate limits or sign-in lockouts hit everyone at once | `HUNTWELL_TRUST_PROXY` unset behind a proxy, so every caller is the proxy's address (the admin sets it on the app VM automatically) |
| A new account never gets its confirmation code | Mail is not sending: check the `notification` service's log in the app VM and the SES keys in the secret. Codes sit in `mail_outbox` until it does |
| Sign-in says "verification failed" for everyone | `TURNSTILE_SECRET_KEY` does not match the widget's site key — the admin log has Cloudflare's error code |
| UI 404 "UI bundle not built" | The binary was built without `UI/web/dist`; run `npm run build` in `UI/web` and rebuild |
