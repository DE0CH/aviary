# Highly available Postgres for YYC: Cluster `pg` to three instances, and `magpie-pg`

**Revised by decision 10 (Deyao, 2026-11-02): the shared Cluster `pg` stays single-instance.** Only section 4
(`magpie-pg` and the `db` NetworkPolicies, including `pg-ingress` excluding `magpie-pg`) and its line of section 8
(EUR 2.0592 a month) still hold. Sections 2, 3, 5-7, the rest of 8 and 9 describe the declined HA plan (EUR 98.904 a
month) and are kept only as its record: no pool `db2`, no `pg` standbys, `pg-hosts` stays `externalTrafficPolicy:
Local` on `10.0.0.2:30432`, and the stated gap of DESIGN 7.3 item 2 stands.

Deyao's decisions 8 (the shared Cluster `pg` made highly available) and 2 (magpie's own three-instance synchronous
Cluster `magpie-pg`), designed against the live cluster read on 2026-11-02 (07:30 UTC) and Hetzner's API
(`GET /v1/pricing`, `/v1/server_types`, `/v1/servers`, `/v1/volumes`, `/v1/placement_groups`, token
`YYC_HETZNER_READ`, project `yyc`). Writing only: nothing here has been applied.

This replaces: DESIGN 7.3 gap 2 and the 7.2 row "Cluster `pg` restart (single instance)", platform 11.5 (the stated
gap) and 16.2's last sentence, and DESIGN 14 Q3 / platform 21.4. It changes hermit 13.5 (the hosts' JuiceFS metadata
address, section 6 here) and adds a few lines to magpie 18.2 (section 4).

---

## 1. What runs today (live, 2026-11-02)

| Item | Live value |
|---|---|
| Operator | CloudNativePG 1.30.1, plugin-barman-cloud v0.15.1 (namespace `cnpg-system`) |
| Cluster `db/pg` | `instances: 1` (`pg-1`), `minSyncReplicas/maxSyncReplicas: 0`, `primaryUpdateMethod: restart`, `podAntiAffinityType: preferred`, nodeSelector `pool=db` + toleration `dedicated=db`, `enablePDB: true` (PDB `pg-primary`, 0 disruptions allowed), image `postgresql:17.11-standard-trixie`, 2 CPU / 6Gi (request = limit), 100Gi `yyc-block`, `max_connections 300`, `shared_buffers 1536MB`, `wal_level logical`, `replicationSlots.highAvailability` on, `failoverDelay 0`, `switchoverDelay 3600`, `smartShutdownTimeout 180` |
| Data | 23 GB used of 98 GB on the volume; `app` 20 GB (the news stream, about 1 GB a day), `cloudcli` 19 MB, `jarvis` 17 MB, `aiwatch` 15 MB, `jfs_yyc_ai` 9.5 MB, `mcp` 8.4 MB |
| WAL | 389 GB written in the cluster's 6 days 5 h (about 0.7 MB/s on average, about 2.5 MB/s at the GIN flushes' 150 MB a minute) |
| Replication | no slots, no publications, no subscriptions (the Neon move's are gone) |
| Clients (by `application_name`) | `yyc-news-stream` 35, `yyc-edge` (Hyperdrive) 20, `jarvis-web` 22, `juicefs` 4, `aiwatch` 4, the singleton locks `yyc-poller-lock`, `yyc-transcribe-lock`, `yyc-whatsapp-lock`, `yyc-whatsapp-wake`, `yyc-live`, `yyc-mcp`, `yyc_readonly` 1 each, `jarvis-egress` 2 |
| Services | `pg-rw` (every in-cluster client; tunnel `yyc-stream` route `db.yyc-internal.com` → `tcp://pg-rw.db…:5432` for Hyperdrive), `pg-r`, `pg-ro` (no endpoints), NodePort `pg-hosts` 30432 with `externalTrafficPolicy: Local`, selector `cnpg.io/instanceRole: primary` (the AWS sandbox hosts' JuiceFS metadata at `10.0.0.2:30432`) |
| Backups | ObjectStore `pg-pitr` (bucket `yyc-pg-pitr`, retention 3d, WAL every ≤ 5 min), ScheduledBackup `pg-nightly` 03:00 HKT; CronJob `pg-dump-daily` 03:30 HKT into the Object-Locked bucket `yyc-pg-backups` |
| NetworkPolicy `db/pg-ingress` | `podSelector: {}` (every pod in `db`): 5432 from `10.244.0.0/16` and `10.201.0.0/24`; all ports from `10.244.0.0/16` |

Nodes (all fsn1-dc14; allocatable after kube/system reserve and eviction threshold; requests from `kubectl describe
nodes`):

| Node | Type | Allocatable | Requested | Free (requests) | Used (`top`) |
|---|---|---|---|---|---|
| `yyc-master1` | cx23 | 2770 Mi | 172 Mi | (tainted, control plane only) | 75 % |
| `yyc-pool-db-worker1` | cx33 | 6701 Mi | 6304 Mi (pg-1 6144 + barman sidecar 128 + CSI/overlay 32) | 397 Mi | 74 % |
| `yyc-pool-apps-worker2` | cx33 | 6701 Mi | 5006 Mi | 1695 Mi | 44 % |
| `yyc-pool-large-worker1` | cpx32 | 6695 Mi | 5312 Mi | 1383 Mi | 49 % |
| `yyc-pool-large-worker2` | cpx32 | 6695 Mi | 2784 Mi | 3911 Mi | 27 % |
| `yyc-pool-small-worker1..5` | cx23 | 2770 Mi each | 2056 / 808 / 786 / 1344 / 1600 Mi | 714 / 1962 / 1984 / 1426 / 1170 Mi | 40-63 % |

Two more `pg` instances at 6Gi each (plus 128 Mi sidecar) need 12.25 GiB placed as two 6.13 GiB slots on two
different nodes, on top of what the nodes run. No node has a 6.13 GiB slot (the largest is 3.8 GiB), and the
apps pool must keep its 2 Gi poller rollout slot and take YYC AI's new stack (about 5.4 GiB during the overlap,
platform 16.2). Hence new nodes.

---

## 2. Nodes: a new pool `db2` of two cpx32 in fsn1

### 2.1 Type

The `pg` instances must be identical (any standby becomes the primary), so each needs 6Gi + 128 Mi + the node
DaemonSets (about 64 Mi with hermit's overlay route beside the old one): a node with at least 6.4 GiB allocatable,
i.e. an 8 GB server. Hetzner's API on 2026-11-02, fsn1, monthly price (the hourly price is capped at it), EUR, the
account's VAT rate 20 %:

| Type | vCPU / RAM | Available in fsn1 | Net / month | Gross / month | Fits one instance |
|---|---|---|---|---|---|
| cx33 | 4 shared / 8 GB | **no** (also no in nbg1, hel1) | 8.49 | 10.188 | yes |
| cax21 (arm) | 4 / 8 GB | no (nor nbg1, hel1) | 10.49 | 12.588 | yes |
| **cpx32** | 4 shared AMD / 8 GB | **yes** | **35.49** | **42.588** | **yes** (6695 Mi allocatable, as large-worker1/2) |
| ccx13 | 2 dedicated / 8 GB | yes | 42.99 | 51.588 | yes, but dearer and fewer cores |
| cpx22 | 2 / 4 GB | yes | 19.49 | 23.388 | no |
| cpx42 | 8 / 16 GB | yes | 69.49 | 83.388 | yes, twice the memory for 1.96× the price; only one instance per node anyway (anti-affinity) |

**Choice: two cpx32 in fsn1**, the cheapest 8 GB type in stock in the volumes' location, the same type as the two
`large` nodes. fsn1 because the volumes are there (a Hetzner volume attaches only within its location) and the
private-network round trip stays inside one data centre (every node is in fsn1-dc14), which is what each synchronous
commit waits for. IPv6-only as every node: the primary IPv6 is free; private-network traffic is free.

A cheaper later cut, not a dependency: if cx33 comes back in stock (`GET /v1/server_types?name=cx33` →
`locations[].available`, `watch-capacity.sh`), a pool `db3` of two cx33 replaces `db2` node by node (section 7.3)
for EUR 20.376 a month instead of EUR 85.176.

### 2.2 hetzner-k3s.yaml (DE0CH/YYC `news-stream/deploy/cluster/hetzner-k3s.yaml`)

A NEW pool, not `db`'s count raised: `db` is cx33 (out of stock) and pool nodes are named by index, so changing the
pool's type around `db-worker1` makes hetzner-k3s retry a missing index for ever (hetzner-k3s skill).

```yaml
  # pool `db2` (nodes yyc-pool-db2-worker1..2; Deyao, 2026-11-02, decision 8: Cluster pg highly available):
  # cpx32 (4 vCPU, 8 GB, EUR 35.49/month net, EUR 42.588 with VAT), the 8 GB type in stock in fsn1 (cx33 was not):
  # one Postgres standby each (../db/postgres.yaml, required anti-affinity: one pg instance per node). Same label and
  # taint as pool db, so the three db nodes are interchangeable to the operator.
  - name: db2
    instance_type: cpx32
    instance_count: 2
    location: fsn1
    labels:
      - {key: pool, value: db}
    taints:
      - {key: dedicated, value: "db:NoSchedule"}
```

Account limits before `cluster.sh create`: the `yyc` project runs 28 shared vCPUs (6 cx23, 2 cx33, 2 cpx32); two
cpx32 add 8 → 36. The account's shared-vCPU limit was raised to 40 on 2026-10-20 (hetzner-k3s skill; cluster
README's "14" is stale) and counts every project of the account, so read Console → Limits first (hetzner-s3 skill
login); a refusal shows as `resource_limit_exceeded` and hetzner-k3s retries for ever. Primary IPs: 10 + 2 = 12 of 16.

### 2.3 Physical spread

hetzner-k3s makes one spread placement group per pool (`yyc-pool-db2-workers-pg1`); `db-worker1` is alone in
`yyc-pool-db-workers-pg1`, and spread groups do not spread across each other, so `db-worker1` could share a physical
host with a `db2` node. After HA is in place (section 7.1 step 6) `db-worker1` joins the `db2` group: Hetzner adds a
server to a placement group only while it is off, which with three instances is a standby's restart.

---

## 3. Cluster `pg`: the target spec

Only the changed or added fields (in `news-stream/deploy/db/postgres.yaml`; the rest stays as it is):

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata: {name: pg, namespace: db}
spec:
  instances: 3                       # pg-1 on db-worker1, two standbys on db2-worker1..2
  primaryUpdateStrategy: unsupervised
  primaryUpdateMethod: switchover    # planned restarts (operator, image, parameters needing a restart, k3s upgrades)
                                     # roll the standbys first, then promote one; the primary is never just stopped
  postgresql:
    synchronous:
      method: any
      number: 1                      # a commit waits for one of the two standbys (synchronous_standby_names = ANY 1 (...))
      dataDurability: preferred      # with both standbys gone, writes go on unsynchronised (today's behaviour), never stop
    parameters: {}                   # unchanged list; no parameter change is needed for replication
  affinity:
    nodeSelector: {pool: db}
    tolerations: [{key: dedicated, operator: Equal, value: db, effect: NoSchedule}]
    podAntiAffinityType: required    # one instance per node (topologyKey kubernetes.io/hostname, the default)
  # minSyncReplicas / maxSyncReplicas: removed (the `synchronous` stanza replaces them; both are 0 today)
---
apiVersion: postgresql.cnpg.io/v1
kind: ScheduledBackup
metadata: {name: pg-nightly, namespace: db}
spec:
  schedule: "0 0 19 * * *"           # 03:00 HKT, unchanged
  target: prefer-standby             # the base backup reads a standby, not the primary the stream writes to
  # … the rest unchanged
```

Why these settings:

- **`any 1` of two standbys**: every acknowledged commit is on two nodes' disks, and one standby can restart, lag or
  die without delaying writes (the other acknowledges). `first 1` would make one named standby the bottleneck.
- **`dataDurability: preferred` for `pg`** (magpie-pg keeps `required`, section 4). The two differ only when both
  standbys are down at once: `required` would stop every write of the news stream, JuiceFS and hermit until one is
  back; `preferred` falls back to today's single-instance behaviour. Decision 8 asks for availability, and the
  stream's data can be re-read from its sources; magpie's transcript writes cannot, so magpie-pg blocks instead.
  `failoverQuorum` stays off for `pg` for the same reason: with the primary and the most advanced standby lost
  together, the operator promotes the remaining one (losing at most what it had not received) rather than waiting
  for a manual promotion.
- **`synchronous_commit` stays `on`** (remote flush). Cost: each commit adds the private-network round trip
  (same data centre) plus the standby's WAL flush to its Hetzner volume. The stream's GIN flushes are throughput,
  not commit-latency bound; measured after step 4 of 7.1. If the stream's commit time is ever the bottleneck, the
  knob is per role: `ALTER ROLE app SET synchronous_commit = local` (the stream's writes async, JuiceFS/hermit/egress
  still synchronous), recorded in postgres.yaml beside the other hand-set role settings. Not set by default.
- **`required` anti-affinity** on the `pool=db` nodes. Memory already allows only one instance per node (6.13 GiB
  of 6.54); the rule makes it explicit if a bigger db node ever joins.
- **Same resources on all three** (2 CPU / 6Gi, 100Gi): a standby is promoted as it is. Standbys need the same
  memory because `shared_buffers` and the replay cache are the primary's after a promotion.
- **`target: prefer-standby`**: the 03:00 base backup reads 23 GB from a standby's volume instead of the
  primary's. WAL archiving stays on the primary (CloudNativePG archives from whichever instance is primary; after a
  failover the new primary continues on a new timeline under the same server name `pg` in `yyc-pg-pitr`).
- Unchanged and right for three instances: `replicationSlots.highAvailability` (the operator's physical slots keep
  the WAL a standby still needs; `wal_keep_size 512MB` beyond), `enablePDB` (with three instances the operator
  keeps a primary PDB and a standbys PDB, so a drain takes one at a time), `failoverDelay 0`, the liveness
  `isolationCheck` (a primary that loses both the API server and every standby stops itself, so a partitioned
  db node never keeps taking writes beside a promoted standby), `wal_level logical` (no slots exist, nothing to sync).

Connections: `max_connections 300` holds on each instance (standbys take no client connections today). The
replication itself takes 2 WAL senders of the default 10 and 2 of the 32 slots.

---

## 4. Cluster `magpie-pg` (decision 2): magpie 18.2 as written, plus these lines

magpie 18.2's manifest stands (3 instances, `any 1`, `dataDurability: required`, `primaryUpdateMethod: switchover`,
10Gi each, 250m / 1Gi, required anti-affinity, the `pg-pitr` ObjectStore under server name `magpie-pg`). Added:

```yaml
spec:
  imageName: ghcr.io/cloudnative-pg/postgresql:17.11-standard-trixie   # the same image as pg, upgraded together
  enableSuperuserAccess: false
  priorityClassName: serving
  postgresql:
    synchronous: {method: any, number: 1, dataDurability: required, failoverQuorum: true}
  affinity:
    nodeSelector: {pool: apps}          # not the db nodes: they hold one 6Gi pg instance each and have 0.4 GiB left
    podAntiAffinityType: required
---
apiVersion: postgresql.cnpg.io/v1
kind: ScheduledBackup
metadata: {name: magpie-pg-nightly, namespace: db}
spec:
  schedule: "0 15 19 * * *"             # 03:15 HKT, after pg-nightly, before the 03:30 dumps
  backupOwnerReference: self
  cluster: {name: magpie-pg}
  method: plugin
  pluginConfiguration: {name: barman-cloud.cloudnative-pg.io}
  target: prefer-standby
  immediate: true
```

- **`failoverQuorum: true`** completes "no acknowledged write lost": the operator promotes a standby only when it
  can show the candidate holds every synchronously committed transaction (with one required acknowledgement among
  two standbys, both standbys must answer). If the primary and a standby fail together, magpie-pg waits for a
  manual promotion instead of promoting a possibly stale standby; magpie answers `unavailable` and the supervisors
  retry, which the design already treats as the safe state (DESIGN 7.2).
- **Placement**: three 1 GiB instances (+128 Mi sidecar each, from the shared ObjectStore's
  `instanceSidecarConfiguration`) on three different `pool=apps` nodes. Free requests today: large-worker2 3.8 GiB,
  small-worker3 1.9 GiB, small-worker2 1.9 GiB, apps-worker2 1.7 GiB, small-worker4 1.4 GiB, large-worker1 1.4 GiB;
  3.4 GiB in three 1.13 GiB slots fits beside the new stack's 5.4 GiB of the 13.9 GiB free and leaves the 2 Gi
  poller slot (platform 16.2). Guaranteed QoS and priority `serving`, as `pg`.
- **Its volumes**: 3 × 10 GB on `yyc-block` (EUR 2.0592 a month, section 8). No new node.

### 4.1 NetworkPolicies in `db`

`pg-ingress` selects every pod in `db` (`podSelector: {}`), and NetworkPolicies add up, so as written it would admit
the whole pod network and the sandbox hosts to `magpie-pg` too, whatever `magpie-pg-ingress` says. Changed:

```yaml
# pg-ingress: every pod in db except magpie-pg's (which has its own policy)
spec:
  podSelector: {matchExpressions: [{key: cnpg.io/cluster, operator: NotIn, values: [magpie-pg]}]}
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: magpie-pg-ingress, namespace: db}
spec:
  podSelector: {matchLabels: {cnpg.io/cluster: magpie-pg}}
  policyTypes: [Ingress]
  ingress:
    # by address, as pg-ingress (k3s adds a new pod to a selector's set only at its next sync; a pod connecting in
    # its first second was dropped, 2026-10-27): the pod network only, never the sandbox hosts (10.201/10.202).
    # The database itself admits only role magpie (REVOKE CONNECT ... FROM PUBLIC), and yyc-ai's egress policy
    # postgres-magpie lets only magpie and the importers out to it.
    - from: [{ipBlock: {cidr: 10.244.0.0/16}}]
      ports: [{port: 5432, protocol: TCP}]   # magpie, magpie-import, pg-dump-daily, the standbys' streaming
    - from: [{ipBlock: {cidr: 10.244.0.0/16}}] # the operator (status 8000) and the instance managers
```

magpie 18.2's narrower form (5432 only from `app in (magpie, import)` and the dump job) would also cut the standbys'
own streaming replication from their primary, and pod selectors hit the k3s sync lag above; the address form is the
working one.

---

## 5. Pooler and Services

- **No CloudNativePG `Pooler` (PgBouncer).** In transaction mode it would break what the clients rely on: the
  singletons' session advisory locks on their own connections (`podlock.py`: poller, transcribe, whatsapp,
  images, youtube), magpie's and hermit's `LISTEN`, the schema-migration lock; in session mode it hides nothing at a
  switchover (the server connection still ends). Hyperdrive already pools the edge site's connections. Every client
  already survives a dropped connection, because a single-instance restart today is the same event (podlock
  re-takes the lock on a new connection within `CHECK` = 5 s, psycopg's pool reconnects, JuiceFS retries its
  metadata transactions, Hyperdrive reconnects). A switchover is that event, shorter.
- **`pg-rw`**: unchanged; the operator points it at whichever instance is primary. Every in-cluster client,
  `pg-dump-daily`, the JuiceFS CSI mounts (`pg-rw.db`) and the `yyc-stream` tunnel route for Hyperdrive stay as they
  are.
- **`pg-ro` / `pg-r`** gain endpoints (the standbys). Nothing is moved to them in this change: read-only clients
  (`yyc_readonly`, `yyc_edge`) would need `hot_standby_feedback` and accept up to a second of lag. A later option.
- **`pg-hosts` (NodePort 30432)**: section 6.
- `magpie-pg-rw` / `-ro` / `-r`: magpie uses `magpie-pg-rw` only (platform 13).

---

## 6. The sandbox hosts' path to JuiceFS metadata (the one client that breaks on a failover today)

`pg-hosts` is `externalTrafficPolicy: Local` and the hosts dial `10.0.0.2:30432` (db-worker1). Local means only the
node running the selected pod answers: once the primary moves to a `db2` node, `10.0.0.2:30432` has no local
endpoint and every host's `/workspace` metadata stalls until the primary comes back. Fix:

```yaml
apiVersion: v1
kind: Service
metadata: {name: pg-hosts, namespace: db}
spec:
  type: NodePort
  externalTrafficPolicy: Cluster   # every node forwards 30432 to the primary, wherever it runs
  selector: {cnpg.io/cluster: pg, cnpg.io/instanceRole: primary}
  ports: [{name: postgres, port: 5432, targetPort: 5432, nodePort: 30432}]
```

- With `Cluster`, a node that receives the connection forwards it and masquerades it to its own pod-network address
  (10.244.x), which `pg-ingress`'s pod-network rule admits. The `10.201.0.0/24` / `10.202.0.0/24` entries stop
  being what admits the hosts (kept until teardown, harmless). What keeps the sandboxes themselves away from 30432
  is then only the hosts' own nftables (a sandbox reaches the egress VIP's 3128 and nothing else, hermit 13.1),
  not this policy: the database admits role `juicefs` only to `jfs_yyc_ai`, with its password.
- **The address the hosts dial** becomes the WireGuard hub's own node, because that node is on every host's path
  anyway (a hub outage cuts the hosts off whatever the address): hermit's hosts `postgres://juicefs@10.0.0.5:30432/
  jfs_yyc_ai` (`yyc-pool-small-worker1`, hermit's gateway node; hermit 13.5's user data and
  `YYC_HERMIT_JUICEFS_META_URL`); the old fabric's new launches `10.0.0.6:30432` (`yyc-sbx-gw` on apps-worker2,
  `YYC_JUICEFS_HOST_META_URL`). Running old hosts keep `10.0.0.2`, which still works through `db-worker1` for as long
  as that node is up (with `Cluster` it forwards to the primary wherever it is).
- Changing `externalTrafficPolicy` reprograms kube-proxy's rules only; established connections keep their
  conntrack entries and go on.

---

## 7. Rollout without downtime for the news stream and YYC AI

Every step is a commit to DE0CH/YYC `news-stream/deploy/` applied the way `yyc-db` is applied (the Flux
Kustomization `yyc-db` today; through the decision-1 workflow if that is extended to it), each after `kubectl diff`
of the rendered `news-stream/deploy/db` against the live state (claude-env "GitOps": plan before a push that
replaces infra). Nodes are added with `cluster.sh create`, as every pool so far. Working hours are fine (decision 9).

### 7.1 Steps

| # | Change | What clients see | Check before the next step |
|---|---|---|---|
| 1 | Pool `db2` (2.2), `SECRETS=… cluster.sh create` | nothing: the taint keeps every pod off; no node is touched | both nodes Ready, label `pool=db`, taint `dedicated=db:NoSchedule`, WARP up (`verify.sh`), `hcloud-csi-node` and the overlay-route DaemonSets running there |
| 2 | Commit A: `pg-hosts` `externalTrafficPolicy: Cluster` (6); `pg-ingress` selector (4.1); `YYC_JUICEFS_HOST_META_URL` → `10.0.0.6:30432` (store + re-seal; read at the next host launch) | nothing (conntrack keeps the hosts' connections) | from an AWS host, a new connection through `10.0.0.6:30432` and through `10.0.0.2:30432` (`juicefs status` / `psql`); `pg_stat_activity` shows `juicefs` sessions |
| 3 | Commit B: `instances: 3` and `primaryUpdateMethod: switchover` only (no affinity or sync change, so `pg-1`'s pod spec does not drift and it is not restarted) | nothing on the primary's connections; load: the operator creates `pg-2`, then `pg-3`, one at a time, each by `pg_basebackup` from `pg-1` (23 GB read from its volume, WAL streamed alongside, so nothing is missed) | `kubectl -n db get cluster pg` 3/3 ready; on the primary `SELECT application_name, state, sync_state, replay_lag FROM pg_stat_replication` = two `streaming` rows (`async`); disk free on `pg-1` |
| 4 | Commit C: `synchronous` stanza, `minSyncReplicas/maxSyncReplicas` removed, `podAntiAffinityType: required`, `pg-nightly` `target: prefer-standby` | the synchronous setting is a reload (no restart); the affinity change is a pod-spec drift, so the operator rolls `pg-2`, `pg-3`, then switches over: one switchover, a few seconds in which connections to `pg-rw` end and reconnect; no acknowledged write is lost | `SHOW synchronous_standby_names` = `ANY 1 ("pg-2","pg-3")` (names as created); `sync_state` = `quorum` on both; the write gap measured (7.2); the stream's lock holders logged `lock connection lost` and re-took their locks (podlock), no second copy ran; the edge site answered; `/workspace` on a host still works |
| 5 | Commit D: `magpie-pg` (4) with `magpie-pg-ingress` and its ScheduledBackup, in P1 commit 1 of the refactor (or before it) | nothing: a new Cluster | 3/3 ready on three apps nodes, `quorum` sync states, first base backup in `yyc-pg-pitr/magpie-pg/` |
| 6 | `db-worker1` into `yyc-pool-db2-workers-pg1` (2.3), once no running host dials `10.0.0.2` (the old fabric's hosts all launched after step 2, or torn down): promote a standby if `pg-1` is primary (`kubectl cnpg promote pg pg-2`, or `status.targetPrimary`), `drain.sh yyc-pool-db-worker1`, `hcloud server shutdown`, `hcloud server add-to-placement-group --placement-group yyc-pool-db2-workers-pg1`, `poweron`, uncordon | nothing (a standby restarting; writes acknowledged by the other) | `pg-1` streaming again, `quorum` |
| 7 | Docs in the same commits: postgres.yaml's header ("One instance: …" becomes the HA paragraph), cluster/README.md (server table, "Adding capacity", the stale 14-vCPU pitfall), hetzner-k3s.yaml's header, drain.sh (a db node: promote away first), the `yyc-news-stream` and `hetzner-k3s` skills' Postgres lines | | |

Order matters at steps 3 and 4: a single commit with `instances: 3` and the affinity change could let the operator
roll the existing single instance (a restart, i.e. today's tens-of-seconds gap) before any standby exists, and
`synchronous` set before the standbys stream would make the first commits wait. Steps 1-3 change nothing a client
sees; step 4's switchover is the only interruption of the whole change and doubles as the failover test.

### 7.2 Measuring the switchover gap (step 4)

A one-off Pod in `db` (image `postgresql:17.11`, role `yyc_readonly`) opens a new connection to `pg-rw` every 200 ms
and logs `now()`, `pg_is_in_recovery()` and failures; the gap is the longest run of failures around the switchover.
Before a planned switchover, a `CHECKPOINT` on the primary shortens it (the old primary's shutdown checkpoint
otherwise writes up to `shared_buffers`' dirty pages, 1.5 GB, to its volume first). The figure goes into
postgres.yaml's header and the `yyc-news-stream` skill.

### 7.3 Afterwards: what node work becomes

- **A db node's reboot, k3s upgrade or replacement**: `drain.sh NODE` (cordon; the operator switches over away
  from a cordoned primary's node; PDBs let one instance go at a time). Writes pause only during a switchover.
- **CloudNativePG or minor Postgres upgrades**: standbys first, then a switchover (`primaryUpdateMethod: switchover`).
- **A db node lost**: if it held the primary, the operator promotes the most advanced standby (seconds after the
  readiness probe fails, `failoverDelay 0`) and recreates the lost instance from the others when the node or a
  replacement returns (its volume reattaches to any fsn1 node). If it held a standby, nothing visible.
- **Replacing `db2` by cheaper nodes** (cx33 back in stock): add the new pool, `drain.sh` one `db2` node, wait until
  its instance runs on a new node (the volume follows), delete the old server; repeat.

What HA still does not cover, stated: the single control-plane node `yyc-master1`: without the API server the
operator cannot fail over; the primary keeps serving (the isolation check needs both the API and every standby
unreachable to stop it), but a db-node loss while the master is down waits for the master. And a loss of fsn1-dc14
as a whole (every node is there).

---

## 8. Cost delta, exact (Hetzner API `GET /v1/pricing`, 2026-11-02, EUR, VAT 20 %)

Volume price: EUR 0.0572 per GB-month net, EUR 0.06864 gross. cpx32 in fsn1: EUR 35.49 net, EUR 42.588 gross per
month (EUR 0.0569 / 0.06828 per hour, capped at the monthly price). Primary IPv6: free. Private-network traffic:
free; 20 TB included per server.

| Item | Quantity | Net / month | Gross / month |
|---|---|---|---|
| Two cpx32 (pool `db2`) | 2 × 35.49 | 70.98 | **85.176** |
| Two `pg` standby volumes | 2 × 100 GB × 0.0572 | 11.44 | **13.728** |
| **Decision 8 (`pg` HA)** | | **82.42** | **98.904** |
| `magpie-pg` volumes (decision 2) | 3 × 10 GB × 0.0572 | 1.716 | **2.0592** |
| **Total new recurring cost** | | **84.136** | **100.9632** |

Rounded as Hetzner's invoice lines: EUR 85.18 + 13.73 + 2.06 per month with VAT. For scale, the cluster's Hetzner
servers and volumes cost EUR 156.06 a month with VAT today (6 cx23 39.528, 2 cx33 20.376, 2 cpx32 85.176, 160 GB of
volumes 10.9824); afterwards EUR 257.03 (156.0624 + 100.9632 = 257.0256).

- **Growth**: `pg`'s storage is now on three volumes, so each GB its volume grows (`resizeInUseVolumes`; the stream
  adds about 1 GB of data a day, 75 GB free today) costs EUR 0.20592 a month with VAT instead of EUR 0.06864.
- **Backups**: unchanged. WAL is archived once (from the primary), base backups are the same size from a standby,
  dumps are taken once; `magpie` adds well under 100 MB to `yyc-pg-pitr` and `yyc-pg-backups`.
- **No other cost**: no load balancer, no floating IP, no Pooler, no extra traffic.
- **Cheaper alternative if cx33 returns to stock**: the two nodes at 2 × EUR 10.188 = EUR 20.376 gross (net 16.98),
  making decision 8 EUR 34.104 gross a month (net 28.42) and the total EUR 36.1632 gross (net 30.136). Today cx33 is
  out of stock in fsn1, nbg1 and hel1.

---

## 9. Changes to the other documents

| Where | Now says | Becomes |
|---|---|---|
| DESIGN 7.2, row "Cluster `pg` restart (single instance)" | stated gap | "Cluster `pg` switchover or failover: seconds in which `pg-rw` connections end; hermit, egress, tools and JuiceFS reconnect; no acknowledged write lost while a standby is up (`any 1`, `preferred`)" |
| DESIGN 7.3 gap 2, platform 11.5, 16.2 last sentence, 21.4, DESIGN 14 Q3 | `pg` single-instance | removed: decided (decision 8) and designed here |
| DESIGN 11 "Databases", platform R31, magpie 18.2 | magpie-pg "pending 14 Q2" | decided (decision 2); plus 4 here (`failoverQuorum`, image, `pool=apps`, ScheduledBackup at 03:15 HKT, the ingress policy's address form) |
| hermit 13.5 user data, platform `YYC_HERMIT_JUICEFS_META_URL` | `10.0.0.2:30432` | `10.0.0.5:30432` (the hub node), after commit A (`externalTrafficPolicy: Cluster`) |
| platform 10.2 "Outside `yyc-ai`" | `db/pg-ingress` gains `10.202.0.0/24` | still added (harmless), but the hosts are admitted through the pod-network rule after the NodePort's masquerade; `pg-ingress`'s selector excludes `magpie-pg` |
| platform 16.4 | one new recurring cost EUR 2.06 | EUR 100.9632 a month with VAT in total (section 8) |
| WP-32 | `magpie-pg` pending | `magpie-pg` decided; plus WP "pg HA" = steps 1-4, 6, 7 of 7.1 (in DE0CH/YYC, before or independent of P1) |
