# Running Huntwell as a fleet

One admin control plane on its own server, many k3d/k3s hosts running the
search plans. This is the shape the `admin` binary was built for.

```
                    people
                      │  https
              ┌───────▼────────┐
              │ load balancer  │        ← the ONLY load balancer
              └───────┬────────┘
                      │
              ┌───────▼────────┐
              │   web tier     │  huntwell serve   (N replicas)
              │  RUN_DISPATCH  │  writes queued executions
              │    = pool      │
              └───────┬────────┘
                      │
                 ┌────▼─────┐
                 │ Postgres │  ← the ONLY thing worker hosts talk to
                 └────┬─────┘
        ┌─────────────┼─────────────┐
        │             │             │   pods poll for work assigned to them
  ┌─────▼────┐  ┌─────▼────┐  ┌─────▼────┐
  │ k3d host │  │ k3d host │  │ k3d host │
  │ hw-pool-0│  │ hw-pool-0│  │ hw-pool-0│
  │ hw-pool-1│  │ hw-pool-1│  │ hw-pool-1│
  └─────▲────┘  └─────▲────┘  └─────▲────┘
        │             │             │   kubectl, outbound from the admin
        └─────────────┼─────────────┘
                ┌─────┴──────┐
                │   admin    │  huntwell admin
                │  (its own  │  reconcile · placement · reaper
                │   server)  │
                └────────────┘
```

## No load balancer reaches the worker pods

Executions do not travel over HTTP. The admin's placement loop writes an
assignment — `(host_id, pod_name)` — onto the queued execution row; each
`hw-pool-N` pod polls for executions assigned to *itself* and claims one
atomically (`cmd/src/worker_pool.rs`). The only Kubernetes Service in the pool
manifest is `hw-pool` with `clusterIP: None`: a headless Service, there to give
the StatefulSet stable DNS identity, carrying no traffic.

That is deliberate, and it is what makes a fleet cheap to run:

- **Worker hosts need no inbound network path at all.** No ingress, no public
  IP, no LB rule per host, no TLS certificate. They need outbound reach to
  Postgres, and that is the whole requirement. A k3d box behind NAT in an
  office works.
- **A pod that dies mid-execution is recovered by data, not by a health check.**
  The heartbeat reaper (default 90s, `HUNTWELL_POOL_STALE_SECS`) fails the
  execution; the StatefulSet recreates the same pod name, so pins survive.
- **Adding a host is one kubeconfig paste.** No routing to reconfigure.

The load balancer in the diagram is for the web tier — the app people sign in
to. It has nothing to do with where executions run.

## Getting the image onto the hosts

The admin deploys *manifests*, not images. `kubectl apply` sends YAML; it
cannot ship a container image, and there is no `k3d image import` across a
network. So every host must be able to pull from a registry.

Run your own on the cluster network with `sys` (`~/Desktop/projects/system`),
which is what [PRODUCTION.md](PRODUCTION.md) does — no account anywhere, and
`sys k8s bootstrap|join --registry ADDR` writes the plain-HTTP trust file on
every node:

```sh
sudo ./sys registry install /opt/huntwell --bind 10.0.0.1 --advertise 10.0.0.1
TAG=0.1.0 ./build.sh --images                   # amd64 by default
sys images push 10.0.0.1:5000 --tag 0.1.0 --via ops@10.0.0.1
```

Or push to a hosted one:

```sh
REGISTRY=ghcr.io/you TAG=0.1.0 ./build.sh --images --push
```

Either way, set the host's **Image** field in the admin dashboard to the pushed
worker name — `10.0.0.1:5000/huntwell-worker:0.1.0`. The five service images are
derived from it, so that one field is the release.

If the registry needs credentials, give them to the admin and it materializes a
`huntwell-registry` pull secret into every host on the next reconcile:

```
HUNTWELL_REGISTRY_SERVER=ghcr.io
HUNTWELL_REGISTRY_USERNAME=...
HUNTWELL_REGISTRY_PASSWORD=...
```

Pull policy is chosen from the image name: `Always` when it has a registry host
(so re-pushing a tag rolls the pool), `IfNotPresent` for a bare name like
`huntwell-worker:dev`, which can only have got there by hand and would sit in
`ErrImagePull` if Kubernetes tried to fetch it. `HUNTWELL_POOL_PULL_POLICY`
overrides.

## The admin server

```sh
./build.sh                              # bin/ubuntu/huntwell, amd64
# copy the binary + a `global` file to /opt/huntwell on the admin server
# install deploy/huntwell-admin.service.in with @APP_DIR@/@APP_USER@ filled in
systemctl enable --now huntwell-admin
```

`sys postgres install --listen ADDR --allow CIDR` is the database under it, if
you are not on a managed one.

It needs `kubectl` on `PATH` — every host operation shells out to it — and
these settings beyond the web app's:

| Setting | Why |
|---|---|
| `HUNTWELL_DATABASE_URL` | the core database, as the *admin* reaches it |
| `HUNTWELL_POOL_DATABASE_URL` | the core database **as the pods reach it** |
| `HUNTWELL_ADMIN_EMAIL` / `_PASSWORD` | the operator login (its own table, not an `Account`) |
| `HUNTWELL_REGISTRY_*` | pull secret, if the image is private |

`HUNTWELL_POOL_DATABASE_URL` is the one that catches people. The admin's own
URL is often loopback or a private address that means nothing inside a remote
cluster; pods that get it start fine and then sit retrying the connection
forever. Set it to the address the clusters can actually dial.

Bind the admin to loopback and reach it through a tunnel, or put it behind a
proxy with its own access control. An operator here can see every workspace's
execution routing and kill any pod in the fleet.

## Registering a host

On the host, get its kubeconfig:

```sh
k3d kubeconfig get huntwell
```

Paste it into the admin dashboard's host form with a pool size and resource
limits. The admin probes it immediately, so a bad kubeconfig is rejected with
kubectl's own words rather than failing quietly later. From then on the
reconcile loop (every 20s, parallel per host) applies:

`Namespace` → `huntwell-secrets` → `huntwell-config` → `huntwell-registry`
(when configured) → headless `hw-pool` Service → `hw-pool` StatefulSet.

The secrets are materialized from the **admin's own** environment, so
`CURSOR_API_KEY` and the Browserbase keys are copied to every host. Each host
holds keys that work for the whole installation — treat a worker host as
trusted infrastructure, not as a customer's machine.

## The web tier

Tick **Run the application here** on a host and the admin deploys it: the
website, planning, scheduling, notification, the bus and one Ingress go to that
cluster alongside the pool, from the same image tag, on the same reconcile
loop. `RUN_DISPATCH=pool` is set for those pods by the admin rather than by
you — it is what makes the deployed website queue executions for placement
instead of forking children inside its own pod, and it is not a thing to get
right by hand. [PRODUCTION.md](PRODUCTION.md) is that path end to end.

A host with the box unticked is pure run capacity and serves nothing.

You can still run the web tier yourself — `huntwell serve`, the single binary,
behind your own load balancer — and the only requirement is the same
`RUN_DISPATCH=pool`. Do that when the app should outlive any one cluster, or
when a load balancer is already terminating traffic somewhere the admin has no
business reaching.
