# Launching Huntwell in production

The whole application runs in **Incus VMs** on bare-metal servers you own, and
the admin control plane drives all of it.

```
 yak-00  10.0.0.1   admin (systemd) + the incus CLIENT only — runs no VMs
          │  Incus API :8443 · a client certificate, trusted once per VM server
          ▼
 yak-01  10.0.0.2   Postgres, and an Incus VM server:
          ├── hw-edge          the one way in: app.yourdomain.com → hw-app
          │                    (Caddy on 80/443, or the one Cloudflare tunnel)
          │                    and 10.0.0.2:4222 → NATS in hw-app, for workers
          ├── hw-app           website · planning · scheduling · notification · nats
          └── hw-yak-01-w1     10 plan slots ─┐
 yak-02 …           more VM servers for more workers ─┤ ← add bare metal here
                    every worker VM joins NATS at 10.0.0.2:4222 (user + password)
```

A server's role decides what you install on it:

| Server runs | Install |
|---|---|
| **VMs** (the app VM, workers, or both) | `sys incus init`, `image`, `token` — step 2 |
| **Only the admin** | the Incus *client* — step 3 |
| **Only Postgres** | `sys postgres install` — step 1 |

The app VM is a VM, so it needs a VM server too. With one VM server it shares
that machine with the workers, and that machine's public IP is the one your
domain points at.

Two things make it work:

- **`sys`** (`~/Desktop/projects/system`) prepares each server: Incus, a VM
  bridge, egress rules, the golden image.
- **The admin** does everything after: trusts the host, provisions VMs, pushes
  executables, deploys, routes the edge, and assigns runs to worker slots.

No registry and no images to push. A deploy is a file push into the
VM and a service restart.

---

## Why the VMs are laid out this way

**2–3 VMs per server** limits what a compromise reaches. Two rules make that
real rather than nominal:

- **A VM carries only its role's credentials.** A worker gets the database, the
  Cursor key and the Browserbase key. It never receives the Cognito admin key,
  the SES key, Stripe, or the session secret — only the app VM holds those. A
  rooted worker costs you run capacity, not your users.
- **A VM reaches only what it needs.** VMs are port-isolated from each other on
  the bridge. The host's egress table lets them reach the internet and the
  destinations you `--allow` (the database and the bus), and no other private
  address — not the Incus API, not another VM's ports.

**One NATS, on the app VM, and every worker VM connects to it.** Runs are still
assigned and claimed through Postgres; workers publish what their runs did —
tokens metered, run finished — so pages update live. Workers reach it at the app
host's private IP, port 4222, which the edge container forwards into the app VM.
It requires TLS — its traffic crosses the network between servers — and a
login, `HUNTWELL_NATS_USER` and `HUNTWELL_NATS_PASSWORD`. The certificates and
the login all live in the secret.

**One way in from the internet**: the app host's edge — one Cloudflare tunnel,
or Caddy on 80/443. Hosts that carry only workers have no edge at all.

---

## 0. Clear anything already on the servers

A server that ran a container orchestrator keeps its bridges, firewall rules and
services, and those fight Incus for the same traffic. On each such server:

```bash
sudo ./sys k8s reset
sudo systemctl disable --now kubelet containerd 2>/dev/null || true
sudo apt-get purge -y kubelet kubeadm kubectl 2>/dev/null || true
sudo rm -rf /etc/cni /opt/cni /var/lib/cni /etc/kubernetes
sudo reboot
```

The reboot clears the kernel modules and forwarding rules nothing else removes.

## 1. The database

Postgres lives outside every VM. On the machine that holds it:

```bash
sudo ./sys postgres install --listen 10.0.0.2 --allow 10.0.0.0/16,10.150.0.0/24
```

**Both networks, even if you only think of the first.** A VM on another server
reaches Postgres NATed through that server's address — covered by
`10.0.0.0/16`. But a VM on the *same* server as Postgres is delivered locally,
never NATed, so Postgres sees its bridge address in `10.150.0.0/24`. Leave that
out and every worker on the database server fails with `no pg_hba.conf entry`,
while workers everywhere else work — which reads like a broken VM, not a
missing rule.

## 2. Prepare each VM server

Only on servers that will run VMs — **not** on the admin server. As root:

```bash
sudo ./sys incus init  --app huntwell --allow 10.0.0.2:5432,10.0.0.2:4222
sudo ./sys incus image --app huntwell        # a few minutes; once per host
sudo ./sys incus token --app huntwell        # prints a one-time token
```

- `init` installs Incus, creates the bridge and the `huntwell` profile, and
  writes the egress rules. `--allow` is every private destination VMs may
  reach — comma-separated: Postgres, and NATS at the IP of the host that will
  carry the app VM. The internet is always allowed.
- `image` builds the golden VM image: Ubuntu, nats-server, Node, the Playwright
  MCP server and the Cursor agent CLI. Re-run it when a tool version changes,
  not when Huntwell changes.
- `token` prints what you paste into the admin. It works once.

On the server that carries the app VM, let the other servers' workers reach
the bus (its own VMs are already allowed):

```bash
sudo ufw allow from 10.0.0.0/24 to 10.0.0.2 port 4222 proto tcp
```

Add servers the same way whenever you need more capacity. A server that also
holds Postgres (yak-01 above) needs nothing extra here — step 1's
`10.150.0.0/24` rule is what lets its own VMs in.

## 3. Start the admin

No environment variables, no service template — the settings chain is Park
River's ([SECRETS.md](SECRETS.md)). `./build.sh` compiles `local-infra/genesis_prod`
(or `.txt`) into the binaries, so no key file goes to the server. The rest is
looked for at or above the executable's folder:

```
/yaksoft/global        encrypted: KEY and SECRET only
/yaksoft/build/ubuntu/ executables the admin pushes into VMs
/yaksoft/bin/admin     the executable, from ./build.sh
```

A release build reads the `Huntwell_Production` secret — nothing to set. It
holds the database connection and every credential, including the bus login:
`HUNTWELL_NATS_USER` and `HUNTWELL_NATS_PASSWORD`, and the bus's TLS
certificates. The app VM will not provision without them.

### The bus's TLS certificates

Once, on your Mac. The server certificate must name the private IP of the
server that carries the app VM (10.0.0.2 here) **and** 127.0.0.1, which the app
VM's own services use:

```bash
mkdir -p ~/keys/huntwell-nats && cd ~/keys/huntwell-nats
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
  -subj '/CN=Huntwell NATS CA' -keyout nats-ca.key -out nats-ca.crt
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -subj '/CN=huntwell-nats' -keyout nats.key -out nats.csr
printf 'subjectAltName=IP:10.0.0.2,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' > nats.ext
openssl x509 -req -in nats.csr -CA nats-ca.crt -CAkey nats-ca.key -CAcreateserial -days 825 \
  -extfile nats.ext -out nats.crt
```

Put three of them in the `Huntwell_Production` secret, as the files' contents:

| Secret key | File | Goes to |
|---|---|---|
| `HUNTWELL_NATS_TLS_CA` | `nats-ca.crt` | every VM, to verify the server |
| `HUNTWELL_NATS_TLS_CERT` | `nats.crt` | the app VM |
| `HUNTWELL_NATS_TLS_KEY` | `nats.key` | the app VM only (0600) |

Paste them with their newlines, as `\n` escapes, or base64 — all three are
read. Keep `nats-ca.key` off every server; it is only needed to issue the next
certificate. If the app VM moves to another server, issue a certificate naming
that server's IP.

A Deploy checks the result: the app VM's website, and a new worker, must log
that they joined the bus, or the Deploy fails saying why.

The admin drives every VM server through the `incus` command, so its server
needs the client — and only the client. `sys incus init` would install a whole
Incus daemon, bridge and firewall rules there, none of which the admin uses. The
client from Zabbly, matching the servers' version:

```bash
sudo mkdir -p /etc/apt/keyrings
sudo curl -fsSL https://pkgs.zabbly.com/key.asc -o /etc/apt/keyrings/zabbly.asc
sudo tee /etc/apt/sources.list.d/zabbly-incus-stable.sources >/dev/null <<EOF
Enabled: yes
Types: deb
URIs: https://pkgs.zabbly.com/incus/stable
Suites: $(. /etc/os-release && echo $VERSION_CODENAME)
Components: main
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/zabbly.asc
EOF
sudo apt-get update && sudo apt-get install -y incus-client
incus version                            # "Server version: unreachable" is expected — no daemon here
```

Set the operator settings once — they live in the database:

```bash
./admin config set HUNTWELL_PUBLIC_URL https://app.yourdomain.com
./admin config set HUNTWELL_MAIL_FROM "Huntwell <hello@yourdomain.com>"
./admin config list                      # confirms the secret was read and the table was written
```

Then run it:

```bash
./admin serve --addr 10.121.17.195:8710  # prints a setup key on first start
```

Open the console over the tunnel or VPN — never the public internet — and claim
it with the setup key.

## 4. Put a build where the admin can push it

On your Mac:

```bash
./build.sh                                  # bin/ubuntu/  (x86_64 hosts)
rsync -a bin/ubuntu admin-server:/opt/huntwell/build/
```

The admin picks each host's executables by its CPU — `build/ubuntu/` for x86_64,
`build/ubuntu-arm64/` for aarch64 — and refuses anything that isn't a Linux
build for that CPU before it creates a VM.

## 5. Add the hosts

**Hosts → Add host** for each VM server: a name (it becomes the Incus remote,
e.g. `yak-01`), its Incus API address (`https://10.0.0.2:8443`), and the token
from step 2. The admin server itself is never added — it runs no VMs.

The admin trusts the host, checks it, and shows its CPU, memory and Incus
version. A bad address or a spent token is refused with Incus's own words, and
nothing is saved.

On the host that will carry the website, also set the **edge**: the domain and
`https` (Caddy on the host's 80/443, getting its own certificates) or `http`
(private network).

For `https`, **point the domain's DNS A record at that server's public IP
first** — Caddy proves it owns the name by answering on port 80, so a record
that is missing or still propagating means no certificate and a site that does
not load. `sys incus init` already opened 80 and 443 in ufw; open them in any
provider firewall in front of the server too.

### Cloudflare edge

Set the host's **Scheme** to `cloudflare`, as in Park River, and its **Domain** to
the site's hostname. The admin then creates a tunnel named
`huntwell-host-<host>`, runs `cloudflared` in the host's edge container beside
Caddy, and points the domain at the tunnel with a proxied CNAME. Nothing listens
on the server's 80 or 443, and you don't edit DNS by hand.

It needs three settings:

```bash
# in the Huntwell_Production secret (it is a credential):
#   CLOUDFLARE_API_TOKEN   Account › Cloudflare Tunnel › Edit, and Zone › DNS › Edit
./admin config set CLOUDFLARE_ACCOUNT_ID <account id>
./admin config set CLOUDFLARE_ZONE_ID <zone id of the domain>
```

Switching an existing host's scheme and saving re-syncs the edge: its published
ports are removed or added to match, and the connector is started or stopped.
Removing the host deletes its tunnel and DNS record.

## 6. Provision the VMs

**VMs → + App VM** on the host with the edge. Then **+ Worker VM** as many times
as you want capacity — leave the host on *least-loaded* and placement spreads
them, respecting each host's VM limit.

A VM takes a few minutes to boot. Its status goes Provisioning → Running, and
its slots appear under its host a moment later. From then on the placement loop
assigns queued runs to free slots.

## 7. Email verification, bot check and rate limits

A new account is emailed a six-digit code and can do nothing but enter it,
ask for another, or sign out until it does — so an invitation sent to an
address goes only to whoever proves they own it, and an address registered by
a squatter is taken over by the first person to verify it. A code is good for
15 minutes and five guesses; "Send a new code" is limited to five an hour.
This needs mail (SES in the secret); nothing else.

Accounts that existed before this was added are treated as verified. A server
with no mail provider (`./dev.sh`) verifies on the spot and says so in the log.


Sign-up and sign-in ask for a Cloudflare Turnstile token when
`TURNSTILE_SECRET_KEY` and `TURNSTILE_SITE_KEY` are in the secret
([SECRETS.md](SECRETS.md)). Set them before opening sign-ups.

The website also limits, per visitor address: ten failed sign-ins in fifteen
minutes locks the address for fifteen; five sign-ups an hour; thirty
invitations a day per account; and eight bad API keys locks the address out of
`/v1` and `/dl`. The address is read from the edge — the last
`X-Forwarded-For` entry from Caddy, or `CF-Connecting-IP` through a tunnel —
which the admin configures on the app VM from the host's scheme, so nothing to
set. The operator console locks an address after five bad passwords.

## 8. First account

Signups are closed. Make the first account from the app VM:

```bash
incus exec yak-01:hw-app -- systemd-run --wait --pipe --collect \
  -p EnvironmentFile=/huntwell/env -p User=huntwell \
  /huntwell/bin/website create-account --email you@yourdomain.com --password '…' --name You
```

`systemd-run` reads `/huntwell/env` exactly as the services do. Sourcing it into
a shell instead would expand any `$` in a password and mangle it.

---

## Operating it

**Deploy a release** — build, copy to the build folder, then **Deploy all**.
The app VM restarts immediately; the edge holds requests through the gap.
Worker slots **roll as their current plan finishes** — a deploy never kills a
running plan. A slot still busy after two hours is stopped.

**Scale** — a worker's **Slots** button changes its concurrency live. Removed
slots finish their current plan first. More VMs: **+ Worker VM**. More machines:
step 2 on a new server, then Add host.

**Take a server out** — edit the host, set it to **Draining**. It keeps its VMs
and takes no new ones. Delete its VMs, then the host.

**When something is wrong**

```bash
incus exec yak-01:hw-yak-01-w1 -- systemctl status 'huntwell-worker@*'
incus exec yak-01:hw-yak-01-w1 -- journalctl -u huntwell-worker@3 -n 50
incus exec yak-01:hw-app -- cat /huntwell/VERSION
```

A stuck slot: the ✕ beside it kills its whole process group; it restarts under
the same name and the run is failed cleanly.

## Inside every VM

```
/huntwell/bin/        executables — pushed by the admin, or from the image
/huntwell/env         settings, root 0600 — the service reads it, cannot read it back
/huntwell/VERSION     the build running (sha256 prefix of its executables)
/huntwell/data/       the only writable directory
```

Every service runs as the `huntwell` user with `ProtectSystem=strict`. Only the
website listens beyond loopback.

## Not built yet

- **More than one app VM.** One per installation, enforced in the database.

## Local development

`./dev.sh` needs none of this: the admin runs worker slots as its own child
processes.
