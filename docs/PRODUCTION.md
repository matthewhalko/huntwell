# Launching Huntwell in production

Two tools, and between them there is nothing to install by hand.

**`sys`** (`~/Desktop/projects/system`) sets up the machines: the registry, the
Postgres, the Kubernetes control plane and each worker node. Copy one binary to
a server and it drives that server's own utilities — no Docker, no packages to
pick.

**The admin** deploys Huntwell onto what `sys` built: the application and the
worker pool both, from one image tag, on a reconcile loop. Registering the
cluster *is* the deploy.

```
   registry 5000 ─┐
   Postgres  5432 ├─ 10.0.0.1   sys registry / postgres / k8s bootstrap
   API       6443 ─┘
                        ▲
  ┌──────────┐  kubectl │
  │  admin   │ ─────────┘        the cluster
  │ (systemd │                   ├─ website · planning · scheduling
  │  its own │                   │  notification · nats    ← the app
  │  server) │                   └─ hw-pool-0..N           ← the workers
  └──────────┘
```

The admin is deliberately not in the cluster it drives: an admin that deploys
itself cannot be trusted to finish a rollout, and an operator who reaches it can
see every workspace's run routing and kill any pod.

---

## What you need first

- **A server for the cluster.** Ubuntu 24.04, x86_64. It can carry the registry
  and Postgres too.
- **A worker node per unit of run capacity.** 8 GB RAM minimum — planning alone
  asks for 3 GB, because it runs the agent. A control plane on its own is
  enough to start; add nodes as runs queue.
- **A server for the admin.** Small. It shells out to kubectl and serves one
  console.
- **Keys**: Cursor, Browserbase, a mail provider.

No public registry account, no `docker login`: `sys` runs a registry on your own
network and `sys images push` fills it.

---

## 1. Put the two binaries on the servers

```bash
cd ~/Desktop/projects/system && ./build.sh          # bin/ubuntu/sys
cd ~/Desktop/projects/huntwell && ./build.sh        # bin/ubuntu/huntwell

scp -p ~/Desktop/projects/system/bin/ubuntu/sys  ops@10.0.0.1:/opt/huntwell/
scp -p ~/Desktop/projects/system/bin/ubuntu/sys  ops@WORKER:/opt/huntwell/
scp -p bin/ubuntu/huntwell ops@ADMIN_SERVER:/opt/huntwell/
```

`sys` roots everything it installs beside itself, so from `/opt/huntwell/sys`
the kubeconfig lands at `/opt/huntwell/k8/kubeconfig/admin.conf` and the images
at `/opt/huntwell/images`. Nothing lands under `/parkriver` unless that is where
you put the binary.

## 2. Build the machines

On the server that holds everything:

```bash
cd /opt/huntwell
sudo ./sys registry install /opt/huntwell --bind 10.0.0.1 --advertise 10.0.0.1
sudo ./sys postgres install --listen 10.0.0.1 --allow 10.0.0.0/24,ADMIN_IP/32
sudo ./sys k8s bootstrap --registry 10.0.0.1:5000 --group huntwell \
     --api-from ADMIN_IP/32 --schedulable
```

Each opens only the adapter you named, to its own subnet, and each explains
itself with `--help`. Three flags carry the weight:

- **`--registry`** writes the trust file for a plain-HTTP registry *before* the
  cluster starts. Without it a node pulls nothing and sits in
  `ImagePullBackOff` against a name that was never going to resolve.
- **`--api-from ADMIN_IP/32`** is what lets the admin reach the API at all. It
  drives this cluster with kubectl from its own server, off the cluster
  network, and the firewall `sys` builds admits the node subnet — not that.
- **`--allow …,ADMIN_IP/32`** on Postgres, for the same reason: the admin
  connects to the database from outside. The pod network is admitted by
  default, which is the other half (pods claim their runs from this database).
- **`--schedulable`** because kubeadm taints the control plane so nothing runs
  on it. On a single machine that leaves every pod Pending for ever. Drop it
  once you have workers and want the control plane kept clear.

Drop both `ADMIN_IP` clauses if the admin lives on the cluster network.

`bootstrap` prints the join line, token included. Run it on every worker:

```bash
sudo ./sys k8s join --server https://10.0.0.1:6443 --token … \
     --ca-cert-hash sha256:… --registry 10.0.0.1:5000
```

The CA hash is what stops a worker trusting whatever answers on that address.
It is also the flag that tells a bare machine which Kubernetes to install, so
the worker sets itself up the same way the control plane did.

Then keep two things from that output — you need them in step 4 and step 5:

- the **kubeconfig** at `/opt/huntwell/k8/kubeconfig/admin.conf` (already
  rewritten to the advertise address, so it works from off the machine)
- the **Postgres password**, printed once by `sys postgres install`

This is upstream Kubernetes: containerd, the packages from `pkgs.k8s.io`,
`kubeadm init`, and flannel for the pod network. `--kube-version v1.35` pins a
different minor; the default is v1.36.

k3s and k3d are development and debugging only — `sys k8s bootstrap --k3s` and
[K3D.md](K3D.md). Nothing in production runs either.

## 3. Build one release and push it into your registry

```bash
cd ~/Desktop/projects/huntwell

ARCH=amd64 ./deploy/images/build-worker-base.sh      # once, when tools change

TAG=0.1.0 ./build.sh --images                       # no target = amd64

sys images push 10.0.0.1:5000 --tag 0.1.0 --via ops@10.0.0.1 \
  --to /opt/huntwell/images --sys /opt/huntwell/sys
```

`build.sh --images` writes six self-contained archives to `bin/images/`, which
is where `sys images push` looks by default. Your laptop cannot usually reach a
registry that only admits the cluster network, so `--via` copies the archives to
that host and pushes from there — one command either way.

**Pick a tag you will never reuse** — a version or a commit sha. Pods pull
`IfNotPresent`, so a node that already holds `0.1.0` keeps running whatever it
first pulled under that name. `sys images push` refuses `dev` and `latest`
outright for this reason.

Only `build-worker-base.sh` needs Docker, and only when a tool version changes;
it pushes to a throwaway local registry, which is where `--images` looks for it
by default. Everything else is zig cross-compilation and crane — no daemon, no VM,
about thirty seconds. The architecture is the point of the bare `./build.sh`:
its default target is Linux amd64, and your Mac is arm64. Getting that wrong is
not subtle — the pod starts, the kernel cannot run the binary, and the log reads
`exec format error`, which does not mention architecture. For an arm64 node,
`./build.sh arm64 --images` and `ARCH=arm64` on the base.

## 4. Start the admin

On the admin server, `/opt/huntwell` holds the binary and its `global` file:

```bash
scp local-infra/global.example ops@ADMIN_SERVER:/opt/huntwell/global
```

Fill it in — the comments in `global.example` cover every setting, and
[SECRETS.md](SECRETS.md) covers reading them from AWS Secrets Manager instead.
The ones that decide whether this works at all:

| Setting | Why |
|---|---|
| `HUNTWELL_DATABASE_URL` | the database, as the **admin** reaches it |
| `HUNTWELL_POOL_DATABASE_URL` | the database, as the **pods** reach it |
| `HUNTWELL_PUBLIC_URL` | links in verification email, and the Ingress hostname |
| `CURSOR_API_KEY`, `BROWSERBASE_*` | the agent and the browser; runs fail without them |
| `HUNTWELL_SESSION_SECRET` | signs session cookies |
| `HUNTWELL_MAIL_*` | without them, verification email goes to a log |
| `HUNTWELL_S3_*` | shared object store for `assets` runs |

`HUNTWELL_POOL_DATABASE_URL` is the one that catches people. Pods claim their
work from the database — that is the only path a run takes to a worker — so if
the admin's own URL is loopback, this must be the address the cluster can dial
(`postgres://…@10.0.0.1:5432/…`), or every pod starts and then retries forever.
`HUNTWELL_POOL_S3_ENDPOINT` is the same trap with the object store.

Install the unit and start it:

```bash
# deploy/huntwell-admin.service.in with @APP_DIR@ / @APP_USER@ substituted
systemctl enable --now huntwell-admin
journalctl -u huntwell-admin -f
```

Starting is the seeding. The admin creates the database if Postgres is empty
(that first start wants a role with CREATEDB), applies the schema — idempotent,
under an advisory lock — prints what it is looking at, then mints a one-time
setup key because no operator exists yet:

```
┌─────────────────────────────────────────────────────────┐
│  This Huntwell control plane has no operator yet.       │
│      HGE5T-3CS3J-X2NDN-PMDHX                            │
└─────────────────────────────────────────────────────────┘
```

Reach the console over an SSH forward — never the public internet — and claim it
with that key. It dies the moment the first operator exists, and a restart mints
a new one, so a key in an old log is already dead.

```bash
ssh -L 8710:127.0.0.1:8710 ops@ADMIN_SERVER    # then http://127.0.0.1:8710
```

## 5. Register the cluster — this is the deploy

In the console: **Register host**.

| Field | Value |
|---|---|
| Name | anything — `prod-1` |
| Kubeconfig YAML | `/opt/huntwell/k8/kubeconfig/admin.conf` from step 2 |
| Pool size | how many runs may execute at once |
| Image | `10.0.0.1:5000/huntwell-worker:0.1.0` |
| **Run the application here** | ✅ |
| Website replicas | 1 |

The admin probes the kubeconfig immediately, so a bad one is rejected with
kubectl's own words rather than failing quietly later. From then on the
reconcile loop (every 20s) applies the cluster's whole desired state:

```
Namespace → huntwell-secrets → huntwell-config → nats
  → website (Service + Deployment) → planning → scheduling → notification
  → Ingress → hw-pool (headless Service + StatefulSet)
```

The other five image names are derived from the worker image you typed — same
registry, same path, same tag, only the name changes. One field, one release:
six names an operator can edit independently is six ways to deploy half a
version.

**Releasing is editing that one field.** Push a new tag with `sys images push`,
change the image on the host, and the next reconcile rolls every service and the
pool together.

## 6. Claim the app

Signups are closed on a deployed host, so make the first account from inside the
cluster rather than opening registration and closing it again:

```bash
kubectl -n huntwell exec deploy/website -- \
  website create-account --email you@yourdomain.com --password 'a-long-password' --name 'You'
```

Then: sign in, create a plan (it should go `drafting` → `ready` within a minute
or two — that is the planning pod claiming it), and run it. `kubectl -n huntwell
get pods -w` shows `hw-pool-N` pick the run up; the console's routing page shows
the same thing as an assignment.

## 7. Get in from the outside

The Ingress routes `/` to the website on whatever hostname `HUNTWELL_PUBLIC_URL`
names. How traffic reaches the cluster's ingress controller is your choice; a
Cloudflare tunnel means no inbound ports at all — run `cloudflared` on the
cluster server with a public hostname pointing at the ingress Service. You can
close everything at the firewall and it keeps working. Cloudflare terminates TLS
and sees your plaintext, which is the trade every tunnel makes.

---

## Adding capacity

`sys k8s join` another node, and that is all — the placement loop spreads
executions across every enabled host and each pod claims only work addressed to
itself. A second *cluster* is a second registered host in the admin; leave **Run
the application here** unticked on it and it is pure run capacity.

Worker hosts need **no inbound network path at all** — no ingress, no public IP,
no TLS, no load-balancer rule. Outbound reach to Postgres is the whole
requirement, so a box behind NAT in an office works. See [FLEET.md](FLEET.md).

## What this does not have

**Backups.** `sys postgres install` gives you a Postgres; nothing here dumps it.
Put `pg_dump` on a timer and **copy it off the machine** — a backup on the disk
it is backing up is not one.

**Disabling a host does not take the app down.** That switch means "place no runs
here"; the application keeps serving. Taking the product offline is not something
a compute-capacity checkbox should do.

**Nothing balances two app clusters.** Two hosts with the box ticked are two
independent copies of the front end. Putting one address in front of both is a
load balancer's job, and it is yours.
