# sandboxd on a host: network, storage, migration, security

The host side of hermit's fabric (hermit spec 13, 14): gVisor (`runsc`, the DE0CH/gvisor fork with memory pre-copy)
sandboxes on cloud VMs behind one WireGuard gateway. sandboxd is the only agent on a host; hermit is its only client.
This note was moved from jarvis3 (`docs/SANDBOX-VM.md`, the sandboxd parts, 2026-11-02, jarvis3 commit 52c9b8c),
with the addresses of the new fabric (hermit spec 13.1) and without a deployment's own names; the measurements are
those of 2026-10-21. The memory bound of sandboxd itself is [MEMORY.md](MEMORY.md).

## Addresses and paths

Every address is the deployment's: sandboxd reads them from host.json (below), which the deployment writes (the
gateway's from hermit's ConfigMap, mounted into its pod, a burst host's from the user data hermit's scaler
gives it, a test hub's through `setup.sh hub`). aviary's scripts carry none. The table shows the layout with
placeholder ranges (the sandboxd-api fixtures use the same); a deployment's own ranges live in its config (YYC's: hermit
spec 13.1, in DE0CH/yyc-code):

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
`imageSource`, `mtu`, `gatewayMss` (gateway: the MSS of SYNs forwarded from wg0 is clamped to it; 1280, 536-9000),
`limits`. A gateway pod's host.json sets `limits.cgroups: false` (the pod's memory limit is its bound; there is no
systemd `Delegate=yes`). Unknown keys are ignored, so a host.json may already carry fields of a later sandboxd
(hermit spec 14.2 lists `egressPorts`; this sandboxd does not read it: the egress port is 3128, hermit spec 13.2
item 5).

- **Sandbox → egress:** sandbox → its host's veth → wg0 → gateway, which DNATs VIP:3128 to one egress pod (`numgen
  random` over the targets hermit pushes with `PUT /v1/egress`) → private network → egress pod. The **source is never
  NATed** towards a pod: egress sees the sandbox's own /32 (it knows a sandbox by its token alone, never by its
  address). A target that is no pod, a load balancer in the private network in front of the proxy (hermit
  `fabric.egressTarget`, the one target pushed), gets the gateway's private address as the source instead, so its
  answers come back whatever its own routes are; the gateway then balances nothing. A moved sandbox's connections
  keep their conntrack entry on the gateway, which is on every path, and survive the move.
- **Direct egress** (host.json `directEgress: true` on a burst host; hermit.json `fabric.directEgress`; off by
  default): the host's sandboxes also reach the internet straight from the host's public address (masqueraded on
  the public interface), beside the proxy. A sandbox's own table then admits, after the proxy and the control
  port, every destination that is the internet and none that is not: the deployment's ranges (`hostCidr`,
  `sandboxCidr`, `controlCidr`, the egress VIP's other ports) and every private, shared, loopback, link-local (the
  cloud's metadata address), benchmark, multicast and reserved range are dropped. IPv4 only: a sandbox has no IPv6.
  The sandbox's rules stay stateless; the host's forward chain lets answers back in by connection state, so a
  direct connection ends when its sandbox moves to another host (its packets would leave from another address),
  while connections through the proxy survive the move as before. A deployment that turns it on therefore keeps
  packing from moving its sandboxes (hermit.json `fabric.migration.minAgeSeconds`). The sandbox needs a resolver
  of its own (the image's `/etc/resolv.conf`): with the proxy alone it never resolved a name. A hub or gateway
  ignores the setting, and so does a sandboxd that predates it (unknown host.json keys are ignored).
- **Host → private network:** what a host itself sends into `controlCidr` (its JuiceFS metadata connections, e.g.
  to a load balancer in front of the database) leaves the gateway with the gateway's private address as the source,
  so the answer needs no route for `hostCidr` at the other end.
- **hermit → sandboxd / sandbox:** pod → node (masqueraded to the node's private address, the `controlCidr` that each
  sandbox's nft lock admits on :7070) → gateway → wg0 → host.
- Burst hosts peer the gateway (its public IPv6, host.json `wgPort`, 51821 in hermit's deployment) and each other (VPC addresses, for host-to-host
  migrations); the gateway learns their endpoints from the handshakes. hermit rewrites the gateway's peer list (each
  host's /32 plus its sandboxes' /32s) after every placement and migration.

## Gateway

sandboxd as the hub with `gateway: true` runs no sandboxes and no runsc (it reports all memory reserved: 0 slots). It
runs as the main process of hermit's gateway DaemonSet (hermit spec 14.2), a host-network privileged pod on one
cluster node, from the image `images/sandboxd` (sandboxd, ip, wg, nft, sysctl, crane, tar, zstd); there is no
installer, no `nsenter` and no host systemd unit. The pod mounts the deployment's host.json (`SBX_CONFIG`), the TLS
files and `wgKeyFile`, and the node's state directory (hostPath, e.g. `/var/lib/hermit-gateway`) at `/var/lib/sbx`.
Its jobs: the WireGuard hub for the burst hosts on its `wgPort` (not 51820, which a cluster's own WireGuard may use),
reached over the node's IPv6; the egress VIP translation; the image source for burst hosts (:7444, filled by hermit's
`POST /v1/images`, crane pulling from the registry).

- **The data plane is the host's, not the process's.** sandboxd leaves wg0, its nft table `ip sbxhost`, its ip rules
  and routes in the host's network namespace when it exits (SIGTERM or a crash; nothing is undone at exit), and at
  start adopts what is there: wg0 is reused (same private key and port: the kernel keeps its sessions), addresses,
  routes and rules are replaced or added only if missing, the nft table is replaced in one transaction (established
  connections keep their conntrack NAT), and the peer set and egress targets hermit last pushed, saved under
  `/var/lib/sbx` (`peers.json`, `egress-targets`), are applied (an equal set changes nothing). A wg0 that is gone (a
  node reboot) is made again with them before hermit's next push. A pod restart or a DaemonSet update therefore stops
  only :7443 and :7444 for the seconds it takes; running sandboxes do not notice. host.json is checked in full before
  anything is touched, so a start with a bad host.json exits and leaves the running data plane as it is.
  `tests/gateway/adopt.sh` checks all of it with the real ip, wg and nft in network namespaces (ci.yml): kill -9 of
  the gateway process under traffic loses no packet and keeps the one TCP connection through the VIP, wg0's ifindex
  and its handshake with the host unchanged; a host.json without `podCidr` leaves everything unchanged; a deleted
  wg0 comes back with the saved peers. With `SBX_GATEWAY_IMAGE` it runs sandboxd from the image (ci.yml `build`).
- Host rules touch only wg0's traffic: nothing from wg0 to the public interface or back, wg0 may reach only
  sandboxd's API and image ports on the node itself, the egress VIP only translated, and SYNs forwarded from wg0 have
  their MSS clamped to `gatewayMss` (1280: the hops behind the gateway, e.g. a 1320-byte pod MTU, may be smaller than
  wg0's 1420). The node's pods, kube-proxy and its CNI keep their own rules and tables (table 51822 and rule priorities
  999/1000/1001 are sandboxd's; no fwmark). Sysctls set: `ip_forward`, and `rp_filter` loose (2) on wg0 and on the private
  interface, where egress pods' replies arrive from pod addresses the node itself routes elsewhere; nothing else.
- **Rule priorities** (2026-11-05): `iif wg0 lookup main suppress_prefixlength 16` at 1000 and `iif wg0 lookup
  51822` at 1001. Both come after the rule for the local table (0 on a plain host; 100 where Cilium moves it, as on
  Talos, whose nodes list `9: fwmark 0x200/0xf00 lookup 2004`, `100: lookup local`, `32500: fwmark 0x40/0x60 lookup
  180` (KubeSpan), `32766: lookup main`), so what comes in on wg0 for the node's own addresses (the API on
  `wgAddress`) is delivered, not routed back out wg0 by the main table's route for `hostCidr`; and before KubeSpan's
  and the main table's. Cilium's own rules sit at 1-112. A start finds these two rules at another priority (an older
  sandboxd's 99/100) and moves them. Before both, `to <podCidr> iif wg0 lookup 51822` at 999: Cilium routes every
  node's pod /24 in the main table (via `cilium_host`, into its VXLAN), which the 1000 rule would pick.
- **Table 51822** holds `podCidr` and `controlCidr` via the private network's gateway, and the node's own pod range as
  the main table routes it (the route inside `podCidr` that holds a local address: Cilium's `cilium_host` /24,
  flannel's `cni0` /24; the private network does not route a node's own range back to it). `controlCidr` is there
  for replies to other nodes (pods arrive masqueraded to their node's private address): main's `/16` for it is
  suppressed by the 1000 rule, and the next match would be KubeSpan's 32500, whose peers drop a source outside the
  node's own addresses, so before 2026-11-05 no AWS host's sandboxd answered hermit.
- **Sandbox sources through a cluster node** (fixed 2026-10-21): a CNI may masquerade every non-pod source going to a
  pod (flannel's FLANNEL-POSTRTG did), so egress would see the node's address. The gateway therefore sends what
  arrives on wg0 to pods over the private network (`ip rule iif wg0 lookup 51822`) and pins the source with an
  identity SNAT in its own table (`keep_source`, priority srcnat - 10: a connection's NAT is bound there, so the
  masquerade never runs).
- **The node's own pods** (fixed 2026-10-21): a pod on the gateway node reaching a sandbox leaves through wg0, where
  a masquerade would give it wg0's address, which the sandboxes' lock drops. `keep_source` SNATs `oifname wg0, pod
  range → sandbox range` to the node's private address.
- **Routes that something else removes** (2026-10-21): systemd-networkd restarted on a node and dropped the routes it
  had not made, cutting every sandbox off. sandboxd checks its routes (and each sandbox's /32) every 10 s and re-makes
  missing ones (logged "host routes were removed"); setup.sh sets networkd's `ManageForeignRoutes=no` /
  `ManageForeignRoutingPolicyRules=no` on the hosts it sets up.

## Gateway on a plain server

The gateway is the one box every sandbox's traffic passes: a cloud network's route has one next hop, so it is single
by nature. It is therefore kept to that one job. `fabric/host/gateway-setup.sh` makes a plain Ubuntu 24.04 server the
gateway: WireGuard, nftables and sandboxd in gateway mode as `sandboxd.service` (`MemoryMax=256M`, restarted for
ever), with the same host.json, WireGuard key and TLS files a gateway pod gets. Nothing else is installed: no
container runtime, no cluster agent, no gVisor, no JuiceFS, no crane (the hosts fetch sandbox images from the
deployment's bucket, hermit `fabric.aws.imageBucket`; the proxy is balanced by a load balancer, `fabric.egressTarget`).

- The server needs the private network (its address there is what the network's route for the hosts' and sandboxes'
  ranges points at) and one public address for the hosts' WireGuard (`wgPort`, udp). Its firewall admits that port
  and nothing else from outside; the API (:7443, hermit's client certificate) is reached over the private network.
- A second run of the script is the upgrade: it installs the binary and host.json it is given and restarts the unit;
  sandboxd adopts the data plane it left (above), so no packet is lost. A host.json that is not a gateway's, or that
  sandboxd refuses, changes nothing. After a reboot the unit makes wg0, the rules and the saved peers again before
  hermit's next push. `sbx-gateway-routes` (the unit's `ExecStartPre`) routes `podCidr` through `privateGateway` in
  the main table: the answers to hermit's API calls, which arrive from pod addresses where the cluster does not
  masquerade them. `tests/gateway/server.sh` checks all of it on a machine with systemd (ci.yml `sandboxd-soak`).
- **Replacing the gateway** (Hetzner): `fabric/host/gateway-move.sh OLD NEW`. NEW is prepared beforehand with
  `gateway-setup.sh`, OLD's WireGuard key and OLD's `peers.json`, with no public address of its own. The script
  powers both off, moves OLD's primary IPv6 to NEW (the address the hosts were given at launch, so running hosts find
  the gateway again by themselves), powers NEW on and moves the network's route. Measured on a replica (2026-11-17,
  cx23 servers): the hosts' traffic stops for 38 to 67 s, then goes on without anything done on a host; the same
  script with the names swapped moves it back. The firewall named in `FIREWALL` is applied to NEW by its id before
  NEW is powered on (a firewall applied by label selector did not attach when the public interface appeared). While it runs every sandbox is without egress and control path, so
  it is run as the node work of the gateway maintenance runbook (`fabric/host/gateway-maintenance.md`).
- **A gateway that was a cluster node** (2026-11-17): a mesh between a cluster's nodes (Talos KubeSpan) lists every
  address of a node as that node's, `wgAddress` on wg0 included, and keeps the list after the node is powered off.
  The other nodes then send hermit's calls to the gateway's API into the mesh, to the old node, whatever the
  private network's route says: the hosts' traffic works, hermit sees no gateway. Before the move, every node that
  reaches the fabric gets a rule ahead of the mesh's (`ip rule add pref 997 to <hostCidr and sandboxCidr> lookup
  main`), and the old node is removed from the cluster after it.

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
- Its files ("Storage"): root = Linux overlayfs (the image read-only + `upper/` on the sandbox's ext4 scratch
  file: the spec's `scratchBytes`, 1 GiB to 1 TiB, 5 GiB when it names none), `/tmp` and `/var/lib/docker` on the
  same scratch. The scratch files are sparse and nothing on the host counts them: the host's disk (hermit.json
  `fabric.aws.diskGiB`, the launch template's volume) must hold every sandbox's scratch in full, which hermit checks
  when it loads its configuration. `/workspace` = `ws/<workspace>` of the host's JuiceFS
  mount (`sbx-jfs`). hermit makes `ws/<workspace>` and sets its quota (hermit spec 15.5); sandboxd makes, chowns and
  quotas nothing there and refuses a create whose folder is missing (409 `workspace missing`). A spec that names
  `drive` also gets `/drive` = folder `<drive>` at the top of the same mount, the same folder in every sandbox of the
  deployment (hermit.json `sandbox.drive`; hermit makes it; 409 `drive missing` otherwise). It travels in the spec, so
  a migration's destination mounts it too. No `~/.claude` bind
  (the supervisor mounts its FUSE filesystem on the pre-made `/home/sandbox/.claude`) and no `/etc/hosts` bind (the
  supervisor writes it).
- The spec's `mounts` (optional, at most 8): the deployment's other persistent folders, each `{folder, path}`: folder
  `<folder>` of the same JuiceFS mount (lowercase names joined by `/`, never `ws` or under it) bound read-write at
  `<path>` in the sandbox, in the spec's order after `/workspace` and `/drive`. sandboxd knows nothing of what a path
  means (a home, a cache): hermit makes each folder with its quota (hermit.json `sandbox.mounts`: folder
  `<name>/<conversation>`, removed with the conversation's workspace) and sandboxd refuses a create whose folder is
  missing (409 `mount missing`). A path is absolute and plain, never on, under or above one of sandboxd's own mounts
  nor on or under `/home/sandbox/.claude`, and no two mounts sit on one another (400 `bad mounts`). The way to each
  mount point is made in the upper layer before the overlay is mounted, like sandboxd's own (`MOUNT_PATHS`), and
  the list travels with the spec to a migration's destination.
- Scratch in use: `Listed.scratchUsed` (bytes, statvfs of the scratch mount, read by the 10 s sampler; absent for a
  receive or an unmounted scratch). hermit moves no sandbox above its `migration.maxScratchMib` (the final delta's
  stopped time, "Scratch on migration").
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

## Freezing (2026-11-17)

- `POST /v1/sandboxes/{name}/freeze` keeps a sandbox, RAM and processes included, off any host (jarvis4
  docs/PLAN-fabric.md, "How a frozen image travels"). Its memory is recorded as a migration would send it: `runsc
  checkpoint --migrate-to 127.0.0.1:<port>` into `sandboxd stream-sink | sandboxd seal | sandboxd s3-put <object>`. Once
  it stopped, its scratch file follows (`tar --sparse | sandboxd seal | sandboxd s3-put <object>.scratch`) and it is
  deleted here. Nothing of its memory is written on the host.
- `POST /v1/sandboxes/{name}/thaw` brings it back on any host: the scratch file first, then network, bundle and
  storage as for a create, then `runsc restore --migrate-listen` with the recording replayed into it (`sandboxd
  s3-get <object> | sandboxd open | sandboxd stream-source`). A failure leaves nothing here.
- Both objects are sealed (seal.rs: AES-256-GCM in 1 MiB frames under the freeze's own key). The request carries the
  key and short-lived credentials for those objects only; each reaches only its own pipeline stage (`seal`/`open`,
  `s3-put`/`s3-get`: SigV4 signed in sandboxd, sent with curl, 64 MiB parts in the stage's own memory, s3.rs). The
  host needs no AWS CLI and holds no credentials of its own.

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
2. **Send, sandbox running:** from here until its entry goes (or the send leaves it running) the source lists it
   `sending: true`, so hermit's settle decides nothing on a source whose send may still switch. Up to 3 rsync rounds of the scratch (`rsync -aHAX --numeric-ids --delete`), stopping
   early once a round copies under 32 MiB and 2,000 files. A failure leaves the sandbox running (409).
3. **Memory pre-copy** (`runsc checkpoint --migrate-to <relay>`). Its final step stops the sandbox and writes its
   dirty file data; the stream ends with End, which the relay keeps.
4. **Sandbox stopped:** the source lists it `alive: false` from here on (hermit's settle never keeps a stopped
   source). The final rsync delta, with `--checksum` (below), while `runsc delete` removes the stopped container (its
   gofer closes the sandbox's files, so JuiceFS commits the last writes to `/workspace` for the target). Then
   `SBXDONE!`: the relay hands End to the restore. A failure here loses the sandbox (502); `SBXFAIL!` makes the
   target's restore fail at once.

Every wait of a receive is bounded, so a target never lists a receive `receiving` for ever (hermit waits while it
does): the relay's limit (900 s from its start) covers its rsync connections (an rsync daemon still running then is
killed) and the stream (checked between reads; a restore that stops reading fails the relay's write after 120 s);
sandboxd kills a relay a minute past that, and a restore that has not finished 300 s after End (it hangs on its gofer
or a mount): the receive is then `failed`.
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
