# Moving YYC's other apps and its Postgres to yyc-talos (T4, T5, T6)

Plan for everything on the old k3s cluster `yyc` that is not the YYC AI stack (whose move is A1-A6, `yyc-ai/cutover`),
under decision 16 (Deyao, 2026-11-04: Claude carries out the full migration; Postgres HA on the new cluster when it
costs under EUR 20 a month; nothing destructive or irreversible without asking him; deleting a server after its
snapshot and releasing a stateless pod need no ask). Written 2026-11-05 from the live clusters (read-only on the old
one), Hetzner's API (`/v1/pricing`, `/v1/servers`, `/v1/volumes`, `/v1/server_types`, token `YYC_HETZNER_READ`,
VAT 20 %) and GHCR's manifests. It follows DESIGN 15 (N10 what the old cluster keeps until A6, N11 the order of T4-T6,
15.5 the bridges, 15.8 `pg-shared` and T5) and MIGRATE.md (the 2026-10-20 move's steps). Nothing here is applied; the
only thing applied to yyc-talos was a throwaway test Postgres (namespace `pg-test`, below), created and deleted with
its two 10 GB volumes the same hour.

Prepared files (none listed by any applied kustomization):

| Path | What |
|---|---|
| `news-stream/deploy/clusters/talos/apps/` | the news stream and the mail merge on yyc-talos: an overlay of `news-stream/deploy/apps`, with a MIGRATION STATE block (everything off until its step) and `wisers-bridge.yaml` |
| `news-stream/deploy/clusters/talos/aiwatch/` | YYC AI watch on yyc-talos: an overlay of `ai-watch/deploy` and `socks-cilium.yaml` |
| `news-stream/deploy/clusters/talos/apps-access.yaml` | namespaces `mailmerge`, `aiwatch`, the monitor's ClusterRole, the rights of `gha:yyc-apps` and the fence's bindings in the three namespaces |
| `news-stream/deploy/clusters/talos/pg/` | Cluster `pg` (replica cluster, then promoted; HA), ObjectStore `pg-archive`, ScheduledBackup, `pg-ingress`, the daily dump, gin-clean |
| `news-stream/deploy/clusters/talos/node-patches/` | Talos patches for the rebuilt x86 nodes: `news.yaml` (pool=news) and `db.yaml` (pool=db, taint dedicated=db) |
| `news-stream/deploy/clusters/hetzner/move-to-talos/` | old-cluster pieces: the relay's NodePorts, `db/pg-shared` alias, the distributed-topology edits of `postgres.yaml`, `copy-volume.sh` |

Every overlay renders (`kubectl kustomize`), `pg.yaml` passes the new cluster's server-side dry-run in both its T5 and
T5-ha states, the node patches pass `talosctl validate --mode cloud` (v1.14.5).

---

## 1. What runs on the old cluster (2026-11-05)

Nodes (all fsn1, x86): `yyc-master1` cx23 (control, tainted), `yyc-pool-db-worker1` cx33 (pool db), `-apps-worker2`
cx33, `-large-worker1`/`-2` cpx32, `-small-worker1..4` cx23. `yyc-pool-small-worker5` is gone: its primary IPv6 and
private address 10.0.0.7 now belong to `yyc-talos-gw1` (cx23, YYC AI's sandbox gateway node, created 03:14 UTC by
another session). Allocatable 40 642 Mi, requested 24 892 Mi.

| Namespace | Workloads (replicas, memory request = limit) | State, hand-over | Talks to |
|---|---|---|---|
| `yyc` (news stream) | Deployments web 2 x 384 Mi (AI server and YYC AI's capture route), flagd 2 x 96, yyc-mcp 2 x 256, cloudflared (tunnel `yyc-stream`) 2 x 128 with its socat sidecar, poller 1 x 2048 (lock `yyc-poller-lock`, maxSurge 1), whatsapp 1 x 256 (lock), transcribe 1 x 640 (lease + lock), tiktok-videos 1 x 768, wisers 1 x 832 (the one WisersOne session, PVC `wisers`), monitor 1 x 512 (no lock, no ingress), images 0 x 1024, three volume anchors 8 Mi each; 20 CronJobs (3 suspended: backfill, broadcast, comments) and `mcp-cleanup` | PDBs: serving minAvailable 1, singletons maxUnavailable 0 (moved only through `cluster/drain.sh`) | Postgres `pg-rw` (db `app`, `mcp`), flagd, wisers :8899, socks pool, YYC AI's ops snapshot (monitor) |
| `mailmerge` | mailmerge 2 x 128, cloudflared (tunnel `mailmerge`) 2 x 128 | stateless | the internet only |
| `aiwatch` | web 2 x 128, worker 2 x 1536 (Chrome; tasks leased from Postgres), cloudflared (tunnel `aiwatch`) 2 x 64 | leases | Postgres (db `aiwatch`), socks pool |
| `egress` | DaemonSet socks (8 copies x 32 Mi, host network, each node's IPv6 + WARP) | stateless | the internet |
| `db` | Cluster `pg` (CloudNativePG 1.30.1, 1 instance `pg-1`, 2 CPU / 6 Gi, 100 Gi), gin-clean 1 x 32, CronJob pg-dump-daily; Services `pg-rw`, `pg-r`, `pg-ro`, NodePort `pg-hosts` 30432 (Local, 10.0.0.2) | PDB `pg-primary` | WAL and base backups to `yyc-pg-pitr`, dumps to `yyc-pg-backups` |
| `jarvis` | YYC AI's old stack only (ui, web, egress, tools, cloudflared): A1-A6's. Its non-YYC-AI parts: none. Two things serve the apps: ConfigMap `yyc/yyc-catalogue` (generated from `yyc-ai/legacy`, read by yyc-mcp; moves with yyc-mcp, DESIGN 15.4) and NetworkPolicy `web-from-news-monitor` (the monitor's snapshot; becomes bridge B4) | | |
| add-ons | cert-manager, cnpg-system (operator 1.30.1, plugin-barman-cloud v0.15.1), kube-system (hcloud CCM and CSI, JuiceFS CSI, Sealed Secrets, CoreDNS), flux-system | | |

Volumes (`yyc-block`, Hetzner, fsn1, Retain; used bytes from the kubelets' stats): `db/pg-1` 100 Gi, 25.89 GB used
(the databases: app 22 GB, cloudcli 20 MB, jarvis 18 MB, aiwatch 16 MB, jfs_yyc_ai 10 MB, mcp 8.4 MB);
`yyc/dumps` 20 Gi, **18.76 GB used**; `yyc/browser-data` 10 Gi, 1.93 GB; `yyc/facebook-data` 10 Gi, 0.24 GB;
`yyc/wisers` 10 Gi, 0.02 GB; `jarvis/jfs` (YYC AI's JuiceFS, not a block volume). Outside Kubernetes: Hetzner volume
`yyc-etcd-snapshots` 10 GB on yyc-master1.

Flux (GitRepository `yyc`, DE0CH/YYC main): `yyc-apps` (`news-stream/deploy/clusters/hetzner`: the apps base, the
StorageClass, Sealed Secrets, the operators), `yyc-apps-secrets` (`news-stream/deploy/apps/sealed`), `yyc-db`
(`news-stream/deploy/db`, prune off), `yyc-aiwatch` + `-secrets` (`ai-watch/deploy`), `yyc-ai` + `-secrets`
(`yyc-ai/legacy`, YYC AI's); HelmReleases cert-manager v1.21.2, cloudnative-pg 0.29.1, plugin-barman-cloud 0.8.1,
juicefs-csi-driver 0.33.0, sealed-secrets 2.20.0.

Tunnels (tofu `news-stream/deploy/cloudflare/tofu/tunnels.tf`): `yyc-stream` (newswatch → socat → web; `/app/wisers`
→ wisers :8899; `mcp.yyc-internal.com` → yyc-mcp; `db.yyc-internal.com` → `tcp://pg-rw.db…:5432` for Hyperdrive),
`mailmerge`, `aiwatch`. A tunnel's connectors move with its apps: connectors on both clusters join the same tunnel, so
every route target must answer on both while they overlap.

### Findings that shape the plan

1. **Every image of these apps is amd64 only** (GHCR, 2026-11-05: yyc-news-stream, yyc-mcp, yyc-socks, yyc-ai-watch,
   mailmerge; no workflow builds arm64), the news stream bundles Google Chrome stable (no Linux arm64 build) and AI
   watch Chrome for Testing linux64. yyc-talos's workers w1-w3 are cax21 (arm64, nbg1). So these apps run only on
   x86 Talos nodes, which are the old servers rebuilt into Talos (fsn1, keeping their primary IPs). The overlays pin
   them with `kubernetes.io/arch: amd64` and `pool: news`.
2. The `dumps` volume is 89 % full (seven nightly pg_dumps of a growing database). The Talos copy asks 40 Gi (question 3).
3. The old `pg-dump-daily` leaves database `aiwatch` out (only WAL/base backups have it). The Talos copy dumps it.
4. `magpie-pg` (`clusters/talos/db/magpie-pg.yaml`) requires region fsn1, but YYC AI's three workers are in nbg1 (and
   cax21 is no longer on sale in nbg1): as written it cannot schedule. YYC AI's session owns that file (section 9).
5. The JuiceFS CSI node plugin requests 384 Mi on every node labelled `yyc.hetzner/network=yyc`
   (measured on yyc-talos-w1), which `patches/worker.yaml` puts on every worker. The x86 app and db nodes therefore get
   no `worker.yaml` labels (`node-patches/`): 384 Mi of each 4 GB node stays with the apps.
6. On yyc-talos, CloudNativePG's `pg-rw` will exist from T5-prep as the replica cluster's read-only designated
   primary. So every app URL on yyc-talos names `db/pg-shared` (bridge B3, re-pointed by T5), never `pg-rw`.
7. The ObjectStore `pg-pitr` of `clusters/talos/db/` (magpie's) limits the WAL sidecar to 128 Mi; Cluster `pg` writes
   about 65 GB of WAL a day at peak and has 512 Mi on the old cluster. `pg/pg.yaml` therefore has its own ObjectStore
   `pg-archive` (same bucket, server name `pg-talos`).
8. The operator path to yyc-talos (`talos.sh tunnel`) goes through yyc-master1, which T6 removes: it needs another
   jump host first (yyc-nat1 has a public IPv6), the builder's tool.

## 2. Order relative to A1-A6

```
A1 ─ A2 ─┬─ A3 ─ A4 ─ A5 ─┬─ A6 ──────────────────────────────────────────────
         │  (freeze, node │
 P1-P4   │  hold: nothing │
 T4a ────┘  moves)        └─ (A5→A6: N10 keeps the old room; nothing of ours)
                                  T4b (news stream) ─ T5 (Postgres) ─ T5-ha ─ T6
```

- **P1-P4 and T4a** (mail merge, AI watch): after A1 (they need bridge B3, `db/pg-shared` and the old `pg-ingress`'s
  10.0.0.0/16 rule, both in A1's first commit) and outside A3-A5 (the window's freeze and node-work hold, N8). They
  can also run after A5; they need no YYC AI step.
- **T4b** (the whole news stream, yyc-mcp, the capture route and the monitor included): after A6. N11 allows these
  three after A5, but until A6 N10 keeps yyc-pool-apps-worker2 and both large workers on the old cluster with room
  for `rollback old-up`, and without those the news stream (about 15.9 GiB at its peak) has no x86 room on yyc-talos.
- **T5** after A6 and T4b (N11: every app that writes to `pg` is on yyc-talos first). **T6** last.

## 3. Nodes per step

Talos allocatable on a 4 GB server is about 3 170 Mi and on an 8 GB one about 7 220 Mi (measured on cax11 3 169 Mi and
cax21 7 221 Mi; x86 is the first rebuild's to confirm). Per-node DaemonSets on the x86 nodes: Cilium 10 Mi, kube-proxy
50 Mi, the socks pool if it runs there, so about 500 Mi kept free each: **2 670 Mi usable on a cx23, 6 720 Mi on an 8 GB
node.**

Demand on x86 Talos (requests): T4a 3 968 Mi (+1 536 Mi rollout surge of an AI watch worker) = 5 504 Mi. News stream:
Deployments 6 808 Mi, CronJobs at most 6 656 Mi at once (each job's request over its longest of the last two runs'
durations, simulated over an hour; average 3 834 Mi), the poller's 2 048 Mi rollout slot, yyc-mcp's 256 Mi surge,
mcp-cleanup 128 Mi = 15 896 Mi. Together 21 400 Mi.

| Step | Old servers rebuilt into Talos at this step | Old cluster afterwards (allocatable / peak need) | x86 Talos usable / used |
|---|---|---|---|
| P2 (after A1, before A3) | small-worker1, -2, -3 (cx23) as `pool=news` | 32 332 / 31 804 (N10's peak, 2026-11-03; 30 012 with today's requests) | 8 010 / 0 |
| T4a | none | 32 332 / 27 836 | 8 010 / 5 504 |
| T4b-serving | none | AW2, L1, L2, S4 22 861 / 16 188 before, 14 460 after (after A6, without the old stack) | 8 010 / 7 488 |
| T4b-L2 | large-worker2 (cpx32) as `pool=news` | 16 166 / 14 460 | 14 730 / 7 488 |
| T4b-jobs | none | 16 166 / 7 780 | 14 730 / 14 168 |
| T4b-L1 | large-worker1 (cpx32) as `pool=news` | AW2 + S4 9 471 / 7 780 | 21 450 / 14 168 |
| T4b-singletons, -wisers, -monitor | none | 9 471 / 2 724 (add-ons; they end on small-worker4) | 21 450 / 21 400 |
| T5-prep | apps-worker2 (cx33) as `pool=db` | S4 (add-ons), M, DB (`pg`) | + db node 1 |
| T5-ha | db-worker1 (cx33) as `pool=db`, 24 h after the promotion | M, S4 | + db node 2 |
| T6 | small-worker4 as `pool=news` (+2 670 Mi); yyc-master1 deleted after its snapshot, or rebuilt if T0's packing wants it | none | 24 120 + |

Old-cluster names: M yyc-master1, DB db-worker1, AW2 apps-worker2, L1/L2 large-worker1/2, S1-S4 small-worker1-4.
small-worker4 stays old until T6 because the old add-ons (operator, Flux, CSI controller, CoreDNS, Sealed Secrets,
about 1.2 GiB) need an untainted node while the demoted old `pg` is kept as the way back. Once everything is on
yyc-talos its x86 pool has 50 Mi spare at the jobs' worst case (2 872 Mi at their average), plus small-worker4's
2 670 Mi after T6 (Claude's arithmetic from the requests above); the two cpx32 cost
EUR 42.588 a month each against EUR 10.188 for a cx33, the obvious thing for T0's packing plan to measure (not part of
this move).

**Rebuilding an old server** (each: no new server, its primary IPv6 and private address kept; decision 16: a snapshot
first makes it reversible, so no ask):
1. `cluster/drain.sh <node>` on the old cluster (singletons hand over through their locks, new pod first), then
   `kubectl delete node`.
2. On the server: `crictl rmi --prune` (the image cache is most of the disk: 22-116 GB used today, 4.3-5.0 GB on the
   nodes without app images), then a snapshot (`hcloud server create-image --type snapshot`; EUR 0.01716/GB a month
   gross, on the image's size), description `k3s <node> before Talos, <date>`.
3. Rename to `yyc-talos-<role><n>` (node name), Hetzner labels `yyc-cluster=talos` (drops it from `yyc-bridge-old`'s
   targets, adds it to `yyc-bridge-new`'s), firewall `yyc-talos` instead of `yyc` (the label selector does it).
4. `hcloud server rebuild --image 451879019` (Talos x86), `talos.sh gen` as a worker with a public interface and a WARP
   key of its own plus `node-patches/news.yaml` or `db.yaml`, `talos.sh apply`, wait Ready.
5. Check: the node's pod-CIDR route in network `yyc` (cloud controller), `kubectl get node -L pool`, no old
   `10.244.x` route left for it.

## 4. Postgres

### 4.1 Which databases, how many instances

- **Cluster `pg`** (namespace `db`): every database of the old Cluster `pg`, copied physically at T5: `app`,
  `aiwatch`, `mcp`, `jfs_yyc_ai`, and YYC AI's `hermit`, `egress`, `tools` (from A1); `jarvis` and `cloudcli` are
  dropped at A6. **Two instances**: a primary and one synchronous standby (`method: any, number: 1,
  dataDurability: preferred`: a commit waits for the standby while it is connected; while it is down or re-cloning,
  the primary carries on alone instead of refusing writes), `primaryUpdateMethod: switchover`, required
  anti-affinity, each on its own dedicated x86 8 GB node in fsn1 (`pool=db`, taint `dedicated=db`): apps-worker2 and
  db-worker1 rebuilt into Talos. Same image, 2 CPU / 6 Gi, 100 Gi and parameters as today (a physical standby needs
  the primary's limits).
- **`magpie-pg`**: as YYC AI designed it (three synchronous instances, 3 x 10 Gi, on YYC AI's workers); unchanged here
  apart from finding 4.
- Not three instances for `pg`: the third would need another 8 GB node and 100 Gi volume, EUR 17.052 a month more
  (EUR 36.1632 with magpie-pg's volumes), over decision 16's EUR 20.

Measured on yyc-talos (2026-11-05, throwaway Cluster `pgt`, 2 instances on two cax21, 10 Gi `yyc-block` volumes,
`dataDurability: preferred`, a writer inserting every 200 ms through `pgt-rw`): both volumes were created in nbg1 by
`WaitForFirstConsumer`; the standby joined as `quorum`; force-deleting the primary pod failed writes for **22 s**, then
the standby was primary, **no acknowledged write lost** (121 acknowledged, all present); a planned switchover failed
writes for **6.6 s**. Cluster, PVCs, PVs, both Hetzner volumes (107369388, 107369393) and namespace `pg-test`
deleted afterwards.

### 4.2 Cost (Hetzner API `/v1/pricing`, 2026-11-05, EUR, VAT 20 %)

| Item | Net / month | Gross / month |
|---|---|---|
| The standby's volume, 100 GB at EUR 0.0572 (gross 0.06864) per GB | 5.72 | 6.864 |
| The standby's node: one 8 GB x86 node the cluster keeps for it (apps-worker2, a cx33, the cheapest 8 GB x86 type: cx33 EUR 8.49 net; on sale in nbg1 and hel1, not fsn1) | 8.49 | 10.188 |
| **Cluster `pg` HA (what HA adds to one instance)** | **14.21** | **17.052** |
| magpie-pg's volumes, 3 x 10 GB (YYC AI's, counted here so the decision covers both) | 1.716 | 2.0592 |
| **Both** | **15.926** | **19.1112** |
| WAL archive, base backups, the second server name in `yyc-pg-pitr` | 0.00 | 0.00 |

Object storage: the account's buckets hold 277 GB today (`rclone size`: yyc-pg-pitr 150.1 GB, yyc-pg-backups
118.7 GB, the rest 8.5 GB) of the 1 TB in the base price; during T5 both server names archive (about another 150 GB
for 3 days), after T6 only `pg-talos`. The primary's own 100 GB volume replaces the old instance's at T6 (no change).
From T5-prep to T6 the new primary's volume and the old one exist together: the extra 100 GB is EUR 6.864 a month
gross, billed for the hours it exists.

### 4.3 The move of the data (zero downtime)

The CloudNativePG distributed topology: yyc-talos's `pg` (self `pg-talos`) starts as a replica cluster of the old one
(self `pg-k3s`), bootstrapped with `pg_basebackup` streamed from the old primary through `pg-hosts`
(10.0.0.2:30432, admitted by the old `pg-ingress`'s 10.0.0.0/16 rule from A1), authenticated as `streaming_replica`
with copies of the old Secrets `db/pg-replication` and `db/pg-ca`, `sslmode=verify-ca`; the WAL archive (server name
`pg`) is its fallback. The promotion is a controlled switchover: the old primary is demoted first and hands over a
token, so nothing committed is lost, and the move reverses the same way until T6.

- **T5-prep** (after T4b; no client changes): rebuild apps-worker2 as db node 1. Seal the Secrets (section 6). One
  commit merges `move-to-talos/pg-topology.yaml` step 1 into `news-stream/deploy/db/postgres.yaml` (old Cluster: the
  `replica` stanza with itself as primary and the two external clusters; check: `pg-1` not restarted, cluster
  healthy). Then a commit lists `pg` in `clusters/talos/kustomization.yaml` (talos-cluster.yml applies it): the
  replica cluster copies 26 GB, at night (the copy reads the whole volume). Check: `kubectl -n db get cluster pg`
  replica healthy, lag in `pg_stat_replication` on the old primary, a base backup under `pg-talos`, and a restore test
  (lessons/82: a second Cluster with `bootstrap.recovery` from `pg-talos`, row counts per table, then deleted with its
  volume).
- **T5** (04:00 HKT; writes pause up to about a minute, the stated gap of DESIGN 15.8):
  1. Every client is on yyc-talos and names `pg-shared` (T4b done; check: `pg_stat_activity` on the old primary shows
     only `streaming_replica` and connections through `pg-shared`'s EndpointSlice from yyc-talos node addresses).
  2. Suspend Flux `yyc-db` (the old side's edits are then made live by `kubectl patch` and committed to git right
     after, so a reconcile changes nothing).
  3. Demote: old Cluster `replica.primary: pg-talos`. The operator stops the old primary cleanly, archives its last
     WAL and restarts it as a replica; `status.demotionToken` appears.
  4. Promote: yyc-talos's Cluster `replica.primary: pg-talos`, `replica.promotionToken: <token>`. Its designated
     primary replays to the token's position and becomes primary.
  5. The commit of DESIGN 15.8 in `clusters/talos/db/pg-shared.yaml`: selector `{cnpg.io/cluster: pg,
     cnpg.io/instanceRole: primary}`, `targetPort: 5432`, EndpointSlice `pg-shared-manual` deleted (talos-cluster.yml
     does not prune: deleted by hand in the same run). Same commit: `pg/pg-dump-daily.yaml` `suspend: false`,
     `pg/gin-clean.yaml` `replicas: 1`; the old `pg-dump-daily` suspended and the old gin-clean at 0 (old `db/`
     commit, Flux resumed).
  6. Check (15.8): exactly one ready address behind `pg-shared`, the new primary's pod on 5432; a write through
     `pg-shared` from a hermit pod, a JuiceFS metadata write from a sandbox host, the news stream's poller writing,
     Hyperdrive's site answering; the old `pg` following `pg-talos` from the archive.
- **Way back** (until T6): steps 3-5 the other way round (yyc-talos demoted with `primary: pg-k3s`, the old promoted
  with its token, `pg-shared` back to the manual slice).
- **T5-ha** (24 h later, after a nightly base backup and dump on yyc-talos succeeded): the way back ends; old Cluster
  `pg` deleted with Flux `yyc-db`'s `prune` (its Hetzner volume stays: Retain); db-worker1 drained, snapshotted and
  rebuilt as db node 2; `pg.yaml` `instances: 2` and the `synchronous` line uncommented: the operator clones the
  standby from the primary (26 GB). Check: `pg_stat_replication` shows the standby `quorum`; a planned switchover.
  EXPENSES.md's Hetzner line gets the standby (volume and node) in this commit.

## 5. The moves, step by step

Mechanics, as MIGRATE.md's rules: anything that serves runs on both clusters while it moves (new copies up and checked
first, then the old ones stop); a singleton never runs twice (poller, whatsapp, transcribe hand over through their
Postgres locks or leases); a CronJob moves only between runs; nothing lands on a cluster that has no room (a
server-side dry run first). Each step is one or two commits: the yyc-talos overlay's MIGRATION STATE loses lines
(applied by `yyc-apps.yml`, P3), the old overlay `clusters/hetzner/kustomization.yaml` gains "replicas 0" / "suspend"
lines (Flux `yyc-apps`). `cluster/migrate.sh` is the tool, changed in P1 for two clusters: `new` runs kubectl with
yyc-talos's kubeconfig, `reconcile_new` waits for the `yyc-apps.yml` run of the pushed commit, volumes copy with
`move-to-talos/copy-volume.sh`, and the jobs step takes a list of CronJobs.

**Preparation**
- P1: migrate.sh for two clusters (above); the store keys and SealedSecrets (section 6).
- P2: rebuild small-worker1, -2, -3 as `pool=news` (section 3); the socks pool (`clusters/talos/socks.yaml`) on them
  too (section 9).
- P3: the apps deploy path: `.github/workflows/yyc-apps.yml` (yyc-apply on `clusters/talos/apps` and
  `clusters/talos/aiwatch`, inventory `yyc-deploy/inventory-yyc-apps`, environment `yyc-apps` with the two Access
  secrets), the authenticator rule for environment `yyc-apps` / workflow `yyc-apps.yml` (the builder's
  `patches/controlplane.yaml`, one control node at a time) and `apps-access.yaml` listed in `clusters/talos/
  kustomization.yaml`. A first run creates everything at 0 replicas, CronJobs suspended. (Without the new identity,
  talos-cluster.yml can apply the two folders instead, but it does not prune or wait for rollouts.)
- P4: `ghcr-pull` in yyc, mailmerge and aiwatch on yyc-talos; a test pod of the news-stream image on a `pool=news` node
  (Chrome starts, the socks pool answers, `pg-shared` answers as `yyc_readonly`).

**T4a: mail merge and AI watch** (after A1, outside A3-A5)
1. mailmerge: the yyc-talos overlay's mailmerge lines go (2 + 2 connectors start there and join tunnel `mailmerge`);
   check both connectors registered (Cloudflare API connections) and a request loop through mailmerge.deyaochen.com;
   then the old ones to 0. No state.
2. aiwatch: web and worker start on yyc-talos (tasks are leased in Postgres: both clusters' workers take tasks
   safely), connectors join tunnel `aiwatch`; check; the old Deployments to 0 (a worker finishes its task, grace
   10 min). Old namespace `aiwatch` and `mailmerge` objects deleted in T6.
3. small-worker4 is NOT rebuilt (section 3).

**A6+: release the 2026-11-05 emergency capacity** (Deyao, 2026-11-05: "the move should have fixed the root cause, so
you need to release the resized servers as well"). On 5 Nov another session (Jarvis 4 session s2023a296073179ae, "YYC
cluster storage capacity") patched the old stack running out of memory: each person's CloudCLI process capped at 1 GB,
`jarvis/ui` pods made bigger, and one or two cpx32 added to the old cluster's pool `large` (or servers resized). All of
it serves only the old `ui` pods, which go to 0 at A5 and away at A6, where each person's process becomes a pigeon
session supervised on the new cluster. Once A6 is done and before T4b's room table is counted:
1. read that session's transcript (or `hetzner-k3s.yaml`'s git log for 2026-11-05) for exactly which servers were added
   or resized. As of 06:30 UTC: `yyc-pool-large-worker1` resized in place cpx32 -> cpx42, disk unchanged (YYC be3b315e);
   no server added; `jarvis/ui` at 1 GB a person, 1664 Mi a pod (2afd282b, gone with the old stack at A6);
2. an added server: drain, `kubectl delete node`, snapshot, delete; its pool's `instance_count` in `hetzner-k3s.yaml`
   back to the value before 5 Nov (in the same commit). A server resized in place: back to its type before 5 Nov
   (`hcloud server change-type`, disk not upgraded, so reversible), drained first;
3. EXPENSES.md: remove those lines. Each released server frees one primary IP and its vCPUs.
If T4b needs x86 room instead (section 3), an added cpx32 may be rebuilt into a Talos `news` node rather than deleted;
that is a capacity decision made then, from the room table, and said to Deyao.

**T4b: the news stream** (after A6)
1. **prep** (old side, additive): `move-to-talos/wisers-nodeport.yaml` into `clusters/hetzner/kustomization.yaml`;
   `move-to-talos/pg-shared-alias.yaml` into `news-stream/deploy/db/`; tofu `tunnels.tf`: `db.yyc-internal.com` →
   `tcp://pg-shared.db.svc.cluster.local:5432` (cloudflare.yml; both clusters answer). On yyc-talos: flagd starts
   (its 2 replicas serve nobody yet).
2. **serving** (zero downtime): web 2, yyc-mcp 2 start on yyc-talos. One push with two commits: YYC AI's
   (`yyc/deploy`) releases its placeholders `yyc/yyc-mcp` and `yyc/web` (`Deploy-Release`), prunes their
   EndpointSlices and moves tools' egress from 10.0.255.1:8081/8082 to the pods (B5, B6 end); ours deletes the
   overlay's two Service delete-patches, so the real Services take the names. Check tools' `web_fetch` and a yyc-mcp
   call from a sandbox. Then the yyc-stream connectors start on yyc-talos (requests now land on both clusters, every
   route answers on both: web, wisers through `wisers-bridge`, yyc-mcp, `pg-shared`); check the request loop through
   newswatch, mcp and a Hyperdrive read; then the old connectors, web, yyc-mcp and flagd to 0.
3. **L2**: rebuild large-worker2 as `pool=news`.
4. **precopy**: `copy-volume.sh dumps pgdump` (18 GB, written once a night), nothing stopped.
5. **jobs**: the CronJobs without a volume (forums, forums-browser, instagram, lihkg, paywall, replies, reports, sites,
   tikhub, watch, mcp-cleanup) move between runs: suspended on the old cluster, each one's running job finishes, then
   unsuspended on yyc-talos. The volume groups next: their old CronJobs suspended, running jobs finish, `copy-volume.sh`
   browser-data (profiles), facebook-data, dumps (uptime.log and the night's delta), the anchors start on yyc-talos,
   their CronJobs unsuspended. Each CronJob keeps the suspend it has in `apps/`. Check every CronJob's next run.
   Jobs on yyc-talos reach the relay through `wisers-bridge`.
6. **L1**: rebuild large-worker1 as `pool=news`.
7. **singletons**: poller, whatsapp, transcribe, tiktok-videos start on yyc-talos and wait on their locks or leases;
   the old ones to 0 and hand over (migrate.sh `singletons`).
8. **wisers**: the old relay drains and saves its session, `copy-volume.sh wisers`, the overlay's bridge lines and
   `wisers-bridge.yaml` go, the relay starts on yyc-talos on the saved session; the old NodePort Service goes. **The one
   gap of the move: about a minute without the relay** (WisersOne allows one session; 2026-10-20 measured 2.5 min
   including a push race), as MIGRATE.md states.
9. **monitor**: the old one to 0, the new one starts (no lock; the page keeps the last published panels meanwhile);
   YYC AI's commit admits the monitor in `hermit-in` and removes bridge B4 (`yyc-bridge-new` then goes with its last
   link). The catalogue generator `yyc/yyc-catalogue` moves to `yyc/deploy` with yyc-mcp's step (DESIGN 15.4).
10. apps-worker2 empty of apps: T5-prep rebuilds it.

## 6. Secrets

Every Secret above is resealed for yyc-talos (`infra/sealed-secrets-talos.pem`) into the overlay's `sealed/`; P1
adds their `NS/NAME@talos` lines to `infra/secrets.txt` and the three folders to seal.py's `SEALED_DIRS` (seal.py
already picks the Talos cert for `news-stream/deploy/clusters/talos/**`).

- Same store keys as on the old cluster: yyc/ai-tools, app-env, ghcr-pull, isp-proxy, mail-account, telegram,
  video-s3, tunnel (same tunnel token: both clusters' connectors join `yyc-stream`), wisers; mailmerge/tunnel;
  aiwatch/ghcr-pull, tunnel; db/pg-role-* and db/pg-backup-s3 (already sealed for yyc-talos in `db/sealed`).
- New store keys, each the old value with host `pg-rw.db.svc.cluster.local` replaced by
  `pg-shared.db.svc.cluster.local` (a script reads the old key from the env and writes the new one with
  `scripts/store-put.py ... KEY=file:...`, never printing it): `YYC_DB_APP_SHARED_URI` (yyc/db-app `uri`),
  `YYC_MCP_SHARED_DATABASE_URL`, `YYC_MCP_OWN_SHARED_DATABASE_URL` (yyc/mcp-env), `YYC_AIWATCH_SHARED_DATABASE_URL`
  (aiwatch/app-env). The relay's host is an env value in the overlay.
- yyc/monitor-keys: `JARVIS_OPS_TOKEN` becomes the hermit monitor token (`HERMIT_ACCEPT_MONITOR`'s value).
- T5: `db/pg-k3s-replication` (tls.crt, tls.key) and `db/pg-k3s-ca` (ca.crt): read from the old cluster's
  `db/pg-replication` and `db/pg-ca` into store keys `YYC_PG_K3S_REPLICATION_CRT`, `_KEY`, `YYC_PG_K3S_CA`
  (`kubectl get secret -o jsonpath | store-put.py`, never printed), then sealed. Deleted with the way back at T5-ha.

## 7. T6: the old cluster goes

After T5-ha. Deletes that need Deyao are marked (D).
1. The operator path: `talos.sh tunnel` through another jump host than yyc-master1 (the builder).
2. Flux suspended, then uninstalled; the old cluster's remaining objects are gone with its servers.
3. small-worker4: drained, snapshot, rebuilt as `pool=news`. yyc-master1 (its etcd snapshots are on volume
   `yyc-etcd-snapshots`): deleted after a snapshot (no ask), or rebuilt if T0's packing wants a fifth cx23.
4. (D) The old Hetzner volumes, kept by Retain: `pvc-b6621635…` (pg-1, 100 GB), `pvc-fd71f9c9…` (dumps, 20 GB),
   `pvc-4d87b283…` (browser-data), `pvc-f9e87338…` (facebook-data), `pvc-046326a5…` (wisers), 10 GB each, and
   `yyc-etcd-snapshots` (10 GB): 170 GB, EUR 11.6688 a month gross while kept.
5. (D) The snapshots taken before each rebuild, once yyc-talos has run a week without needing one.
6. Repo: `news-stream/deploy/clusters/hetzner/`, `cluster/` (hetzner-k3s, cluster.sh, drain.sh, migrate.sh),
   `news-stream/deploy/db/` (moved into `clusters/talos/pg/`), `ai-watch/deploy`'s Flux files, the overlays'
   MIGRATION STATE blocks and `move-to-talos/` deleted (one clean-cut commit); the apps base folders become the
   yyc-talos folders' only source; the skills (`yyc-news-stream`, `hetzner-k3s`) and EXPENSES.md's Hetzner lines
   describe the one cluster. Hetzner firewall `yyc` deleted.

## 8. What needs Deyao

- Nothing before T6 needs his approval: the HA spend is inside decision 16 (EUR 19.1112 of EUR 20 with magpie-pg),
  rebuilds are snapshot-first, the T5 switchover is reversible until T5-ha. He is told (Discord) before T5 runs and
  when T6's deletes are ready.
- T6 items 4 and 5 (deleting volumes and snapshots) need his OK.
- Question 3 below (the dumps volume, a new spend of EUR 1.3728 a month).

## 9. For the other sessions (read from here, not messaged)

- **YYC AI** (owner of `yyc/deploy`, `clusters/talos/db/`): finding 4 (`magpie-pg` requires fsn1, workers are in
  nbg1); T4b-serving's commit releasing `yyc/yyc-mcp` and `yyc/web` and moving tools' egress; T4b-monitor's
  `hermit-in` rule and B4's removal; `pg-shared`'s T5 commit (15.8) is the one in section 4.3 step 5.
- **The builder** (`talos.sh`, `patches/`, `nat/`): a worker role with a public interface and WARP but without
  `worker.yaml`'s labels, for `node-patches/news.yaml` and `db.yaml`; the `yyc-apps` authenticator rule (P3); the
  jump host for T6.
- **The plumbing session** (socks pool, `clusters/talos/socks.yaml`, DaemonSet on `pool: apps` +
  `yyc.hetzner/network: yyc`): the `pool=news` nodes carry neither label, so the pool runs only on YYC AI's workers
  and every news-stream exit would be yyc-nat1's one WARP address; the old pool exits from each node's own IPv6 and
  WARP. Adding the `pool=news` nodes to the DaemonSet (they have a public IPv6 and their own WARP) keeps that. AI
  watch's policy admits port 1080 to nodes and to namespace `egress` pods (`aiwatch/socks-cilium.yaml`).

## 10. Questions for Deyao

1. A temporary node during T4b would make the steps less tight (section 3 leaves 50 Mi of headroom at T4b's end with
   the jobs' worst case, about 2.8 GiB with their average). Not planned: the brief says nodes come from rebuilds only.
   If wanted: a cx33 in nbg1 without a public IP, EUR 0.0136 an hour net, EUR 0.01632 gross (capped at EUR 10.188 a
   month gross), deleted after T5.
2. After the move, the two cpx32 (EUR 42.588 a month each) could be replaced by cx33s (EUR 10.188) without a public
   IP behind yyc-nat1; that is T0's packing, measured after the move, not part of it.
3. The `dumps` volume on yyc-talos: 40 Gi instead of 20 Gi (it is 89 % full), EUR 2.7456 a month instead of EUR 1.3728.
