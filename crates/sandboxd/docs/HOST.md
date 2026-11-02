# sandboxd on a host: network, storage, migration, security

The host side of hermit's fabric (hermit spec 13, 14): gVisor (`runsc`, the DE0CH/gvisor fork with memory pre-copy)
sandboxes on cloud VMs behind one WireGuard gateway. sandboxd is the only agent on a host; hermit is its only client.
This note was moved from jarvis3 (`docs/SANDBOX-VM.md`, the sandboxd parts, 2026-11-02, jarvis3 commit 52c9b8c),
with the addresses of the new fabric (hermit spec 13.1) and without a deployment's own names; the measurements are
those of 2026-10-21. The memory bound of sandboxd itself is [MEMORY.md](MEMORY.md).

## Addresses and paths

Every address is the deployment's: sandboxd reads them from host.json (below), which the deployment writes (the
gateway's from hermit's ConfigMap through `fabric/host/gateway.sh`, a burst host's from the user data hermit's scaler
gives it, a test hub's through `setup.sh hub`). aviary's scripts carry none. The table shows the layout with
placeholder ranges (the sandboxd-api fixtures use the same); a deployment's own ranges live in its config (YYC's: hermit
spec 13.1, in DE0CH/YYC):

| What | host.json | Example (placeholders) |
|---|---|---|
| Hosts on wg0 | `hostCidr`, `wgAddress` | 10.99.0.0/24 (gateway .1, burst hosts .10-.250) |
| Egress VIP | `egressVip` (tcp 3128 only) | 10.99.1.1 |
| Sandboxes | `sandboxCidr` | 10.98.0.0/16, one fixed /32 each, which follows it across hosts |
| The private network's route to the gateway node | (the cloud network's, `gateway-net.sh`) | 10.98.0.0/15 |
| hermit's sources (the nodes' private addresses) | `controlCidr` | the cluster's private network |
| Pods (egress targets) and their next hop | `podCidr`, `privateGateway` | the cluster's pod range and private gateway |

sandboxd compiles none of them in: `hostCidr`, `controlCidr`, `sandboxCidr` and `egressVip` are required on every
host, `privateGateway` on a hub, `podCidr` on the gateway, and `imageSource` on a host without registry access (a
burst host); a host.json without them is refused at start (400). A registry pull needs the image's reference (`ref`,
or the spec's `imageRef`): there is no default image.

### host.json

`/etc/sbx/host.json` (`SBX_CONFIG` to point elsewhere), parsed by `src/config.rs`: `name`, `role` (`hub` | `burst`),
`gateway` (hub only: no sandboxes), `wgAddress`, `hostCidr`, `wgPort`, `wgKeyFile`, `apiPort` (7443), `imagePort`
(7444), `tls` {`ca`, `cert`, `key`}, `controlCidr`, `sandboxCidr`, `egressVip`, `podCidr`, `privateGateway`, `hub`
(burst: the hub peer {`publicKey`, `endpoint`, `allowedIps`}), `reserveMemory` (bytes), `registryConfigDir`,
`imageSource`, `mtu`, `limits`. Unknown keys are ignored, so a host.json may already carry fields of a later sandboxd
(hermit spec 14.2 lists `egressPorts`; this sandboxd does not read it: the egress port is 3128, hermit spec 13.2
item 5).

- **Sandbox → egress:** sandbox → its host's veth → wg0 → gateway, which DNATs VIP:3128 to one egress pod (`numgen
  random` over the targets hermit pushes with `PUT /v1/egress`) → private network → egress pod. The **source is never
  NATed**: egress sees the sandbox's own /32 and accepts its token only from there. A moved sandbox's connections keep
  their conntrack entry on the gateway, which is on every path, and survive the move.
- **hermit → sandboxd / sandbox:** pod → node (masqueraded to the node's private address, the `controlCidr` that each
  sandbox's nft lock admits on :7070) → gateway → wg0 → host.
- Burst hosts peer the gateway (its public IPv6, host.json `wgPort`, 51821 in hermit's deployment) and each other (VPC addresses, for host-to-host
  migrations); the gateway learns their endpoints from the handshakes. hermit rewrites the gateway's peer list (each
  host's /32 plus its sandboxes' /32s) after every placement and migration.

## Gateway

sandboxd as the hub with `gateway: true` (`fabric/host/gateway.sh`, a systemd unit on a cluster node, installed by
hermit's gateway DaemonSet, hermit spec 14.2) runs no sandboxes and no runsc (it reports all memory reserved: 0
slots). Its jobs: the WireGuard hub for the burst hosts on its `wgPort` (not 51820, flannel's own WireGuard), reached over
the node's IPv6; the egress VIP translation; the image source for burst hosts (:7444).

- Host rules touch only wg0's traffic: nothing from wg0 to the public interface or back, wg0 may reach only
  sandboxd's API and image ports on the node itself, the egress VIP only translated. The node's pods, kube-proxy and
  flannel keep their own rules; no packages, sysctls (beyond ip_forward) or nftables.service are touched.
- **Sandbox sources through a k3s node** (fixed 2026-10-21): flannel masquerades every non-pod source going to a pod,
  so egress would see the node's flannel address. The gateway therefore sends what arrives on wg0 to pods over the
  private network (`ip rule iif wg0 lookup 51822`) and pins the source with an identity SNAT in its own table
  (`keep_source`, priority srcnat - 10: a connection's NAT is bound there, so the masquerade never runs).
- **The node's own pods** (fixed 2026-10-21): a pod on the gateway node reaching a sandbox leaves through wg0, where
  flannel's masquerade would give it wg0's address, which the sandboxes' lock drops. `keep_source` SNATs `oifname wg0,
  pod range → sandbox range` to the node's private address.
- **Routes survive systemd-networkd** (2026-10-21): networkd restarted on a node and dropped the routes it had not
  made, cutting every sandbox off. gateway.sh and setup.sh set `ManageForeignRoutes=no` /
  `ManageForeignRoutingPolicyRules=no`, and sandboxd checks its routes (and each sandbox's /32) every 10 s and re-makes
  missing ones (logged "host routes were removed").
- wg0 and the nft table survive a sandboxd restart; sandboxes do not notice one.

## sandboxd

- API: crate `sandboxd-api` (HTTPS on the wg0 address, port 7443, TLS 1.3, mutual TLS, client CN `hermit` only).
- State per sandbox in `/var/lib/sbx/sandboxes/<name>.json` (mode 600, written atomically). The unit has
  `KillMode=process`: restarting or upgrading sandboxd never touches sandboxes (checked: setup.sh re-run with two
  sandboxes running, same PIDs).
- Every sandbox: `runsc --platform=systrap --network=sandbox --overlay2=none --host-uds=none --host-fifo=none
  --net-raw --allow-packet-socket-write --allow-suid`, the pool's CPU features (annotation), cgroup `/sbx/<name>`
  (memory and CPU quota from its spec, pids from its spec or 4096), its own netns. The first process (uid 0, cwd
  `/work`) is the image config's `Entrypoint` + `Cmd`; an image without an entrypoint is refused (400). The spec has
  no `args` or `user`: an unknown spec field is a 400. `/dev/fuse` (`c 10:229`, 0666) is in the bundle's
  `linux.devices` and the device allow-list; runsc needs no FUSE flag (the fork registers FUSE unconditionally), so
  the flags above are the same on every host.
- Its files ("Storage"): root = Linux overlayfs (the image read-only + `upper/` on the sandbox's 5 GiB ext4 scratch
  file), `/tmp` and `/var/lib/docker` on the same scratch, and `/workspace` = `ws/<workspace>` of the host's JuiceFS
  mount (`sbx-jfs`). hermit makes `ws/<workspace>` and sets its quota (hermit spec 15.5); sandboxd makes, chowns and
  quotas nothing there and refuses a create whose folder is missing (409 `workspace missing`). No `~/.claude` bind
  (the supervisor mounts its FUSE filesystem on the pre-made `/home/sandbox/.claude`) and no `/etc/hosts` bind (the
  supervisor writes it).
- Liveness: `Listed.alive` is `populated 1` in `/sys/fs/cgroup/sbx/<name>/cgroup.events`, read by the 10 s sampler
  (no runsc call); a sentry the host's memcg OOM-killed shows `alive:false` within 10 s. A receive is always alive.
- Console: `GET /v1/sandboxes/<name>/log?tail=<bytes>` answers `text/plain`, the last bytes (at most 64 KiB) of
  `/var/log/sbx/<name>.log`, while the sandbox is here and for 10 min after it ended (from the log's mtime, which the
  delete sets); 404 otherwise.
- `GET /v1/host` reports `jfsMounted` from `/proc/self/mountinfo` (never a stat of the FUSE root, so a wedged juicefs
  cannot stop the endpoint answering; null on a host without `/etc/sbx/juicefs.env`, the gateway). A sandbox with a
  workspace waits up to 30 s for the mount before it is refused (503).
- `sbx-jfs.service` starts after sandboxd: it reaches the metadata store over sandboxd's WireGuard, and first waits up
  to 30 s for a wg0 route sandboxd adds (not the kernel's connected route, which appears at `link up` before the route
  to the control network).
- Host setup: `fabric/host/setup.sh hub|burst [image]` (idempotent: the fork's runsc checked against SHA256SUMS,
  crane, sandboxd from `bin/` (`fabric/host/fetch-sandboxd.sh`: aviary's release `aviary-<sha12>`, checked), sysctls,
  the tmpfs for migration images, units, logrotate, the image unpacked). Burst images get each instance's host.json,
  WireGuard key and TLS files from EC2 user data at first boot (`sbx-userdata.service`). The user data's `host` must
  set `imageSource` to the gateway's image server, `http://<gateway wgAddress>:<imagePort>`: sbx-userdata refuses
  user data without it, because sandboxd's compiled-in fallback is the old fabric's hub (README.md).
- PKI: `fabric/host/make-certs.sh` (CA "hermit fabric CA", the gateway's and the burst hosts' server certificates
  "sandboxd", hermit's client certificate CN `hermit`).

## Capacity

- **Everything a sandbox runs is inside its cgroup** `/sbx/<name>` with the sandbox's limit: the runsc `exe`, the
  sentry and the gofer, the sandbox's memory (a memfd), and the kernel memory and page tables they cause (idle
  sandbox: ~40 MiB in total). Outside it: only runsc's `runsc-fd-parking` sidecar (~8 KiB, in sandboxd's unit) and the
  netns/veth/nft objects.
- **Isolation** (`setup.sh`): sandboxd's unit has MemoryMin 160M (system.slice gets the same so it takes effect),
  MemoryHigh 384M and MemoryMax 512M; `/sbx` is capped at MemTotal − reserveMemory (`sbx-cgroup`, run before each
  sandboxd start), so sandboxes can never take the host's reserve and sandboxd can never take theirs. The limits sit
  on the unit, not on a slice of its own: the fd-parking sidecars stay in the unit's cgroup (KillMode=process), so
  systemd could never move the unit to another slice while sandboxes run.
- **A migration receive** joins the new sandbox's cgroup before it receives and writes the pages into that sandbox's
  `memory.img` (tmpfs, its memory afterwards): it is charged to the incoming sandbox's own limit.
- **Over its limit a sandbox dies whole.** gVisor reports the cgroup limit as the sandbox's memory but has no OOM
  killer of its own: when sentry + gofer + memory reach the limit, the host's memcg OOM killer kills the sentry. What
  the sandbox can use is about the limit minus 50-70 MiB.

## Scratch on migration (2026-10-21)

A sandbox's scratch filesystem (`/var/lib/sbx/scratch/<name>`: the overlay's `upper/`, `/tmp`, Docker's directory) is
host disk, not memory, so it moves as files, with rsync, around the memory pre-copy (`storage.rs`, `relay.rs`,
`sandboxes.rs`):

1. **Receive:** the target prepares the same storage as a create and the bundle, starts `runsc restore
   --migrate-listen 127.0.0.1:<port+1000>`, and `sandboxd migrate-relay` (a helper process in the helpers cgroup) on
   `<overlay address>:<port>`, the port hermit gets. The relay serves the source's connections one at a time: rsync
   (`rsync --daemon` in inetd mode, module `scratch`, chroot, overlay addresses only), the pre-copy stream (relayed
   record by record, except its End record, which it holds back), and `SBXDONE!`/`SBXFAIL!` from the source.
2. **Send, sandbox running:** up to 3 rsync rounds of the scratch (`rsync -aHAX --numeric-ids --delete`), stopping
   early once a round copies under 32 MiB and 2,000 files. A failure leaves the sandbox running (409).
3. **Memory pre-copy** (`runsc checkpoint --migrate-to <relay>`). Its final step stops the sandbox and writes its
   dirty file data; the stream ends with End, which the relay keeps.
4. **Sandbox stopped:** the source lists it `alive: false` from here on (hermit's settle never keeps a stopped
   source). The final rsync delta, with `--checksum` (below), while `runsc delete` removes the stopped container (its
   gofer closes the sandbox's files, so JuiceFS commits the last writes to `/workspace` for the target). Then
   `SBXDONE!`: the relay hands End to the restore. A failure here loses the sandbox (502); `SBXFAIL!` makes the
   target's restore fail at once.
5. **The upper directories on the way to the bind mounts** (`/tmp`, `/var/lib/docker`, `/workspace`,
   `/home/sandbox/.claude` and their parents) are made in `upper/` before the overlay is mounted, on every host:
   overlayfs caches a directory it found only in the image, and the bind mount pins it.

Preserved: contents, numeric owners, modes, mtimes to the nanosecond (gVisor's restore checks size and mtime of every
open file), hard links, ACLs, xattrs (`trusted.overlay.*`) and devices (whiteouts). Measured on two m7i.large in one
AZ: scratch live rounds 0.7-0.8 s for 230 MB, `send` 1.9 s, final memory 190-370 ms, final scratch delta 130 ms,
restore 59-67 ms; the largest gap a busy process saw: 330-800 ms.

The final delta compares contents (`rsync --checksum`), not size and mtime: a write through a `MAP_SHARED` mapping to
a page that is already dirty leaves the file's mtime unchanged, so the quick check skips the file and the target
restores an older copy (the supervisor's FUSE journal slots, SQLite's `-shm`; crates/supervisor/GATE.md "WP-57").
It reads every scratch file on both hosts while the sandbox is stopped: measured on two Hetzner ccx13 hosts
(`tests/real-host/`), 129-139 ms stopped with a near-empty scratch, 5.9 s with 2000 MiB in use and 15.5-15.8 s with
4800 MiB (the quick check: 129-147 ms, and it lost the mapped writes in every run), about 3.2 ms per MiB in use.

What moves: processes (same PIDs), the scratch filesystem and tmpfs mounts, memory, established TCP connections
(gVisor's own stack, `--network=sandbox`), Docker state inside the sandbox. What does not: connections from outside in
their handshake at the switch (they wait for a SYN retransmission, ~1 s).

## The fork (DE0CH/gvisor)

- **Dirty tracking** without host kernel features: while a round runs the sentry maps the sandbox's memory
  read-only; the first write to a page faults into the sentry, which marks it dirty and maps that page writable. The
  sentry's own writes are marked where they happen. Each round's dirty set is a bitmap per memory file; nothing is
  tracked when no migration runs.
- **The TCP bug:** upstream, established TCP connections survived a sandbox's first checkpoint/restore and were
  aborted on the next (`allowLiveTCPMigration` was not saved). The fork saves it (commit `913bf5cf9`).
- **The close-flush fix** (`fabric/gvisor-patches/0001-gofer-close-flush-error.patch`): with directfs a FUSE file's
  close never reached the host, so a quota error on close was lost; the patch closes a duplicate of the host fd in
  OnClose and returns its error.
- **CPU features across clouds:** gVisor refuses to restore a sandbox whose saved CPU features the new host lacks, so
  every sandbox starts with the pool's common feature set (annotation `dev.gvisor.internal.cpufeatures`). Adding a
  host type changes the common set: sandboxes started with the old set may not fit it.

## Security

- gVisor is the boundary: the sandbox's syscalls go to the sentry, which runs under its own seccomp filter, in an
  empty mount namespace, with no host capabilities. The fork writes the migration stream to a socket runsc passes in
  (no new syscalls in the sentry's filter).
- uid 1000 inside, with sudo to the sandbox's root (whose capabilities exist only inside gVisor); `--host-uds=none
  --host-fifo=none`; cgroup limits for CPU, memory and pids; no host mounts but the JuiceFS folders; its own netns with the
  stateless nft lock.
- Migration streams run over WireGuard between hosts; the receiver accepts connections from overlay addresses only,
  for an expected migration.
- AWS hosts: no instance profile, IMDSv2 only with hop limit 1 and the metadata address dropped from every sandbox, no
  key pair (the AMI bake removes its build key).
