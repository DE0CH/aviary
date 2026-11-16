# Remaining refactor work, owned by this session (Deyao, 2026-11-04: no more workflows; Claude does it directly)

Order (each item: code on main, tests on the runner, CI green, then the next):
1. WP-50 pigeon legacy upstream: land the interrupted fix in ~/workspace/wt/WP-50-aviary (9 files), close its open
   review items (late claim answer, buffered bytes after bound, rewrite accepted:false, tail/heartbeat/drain/ready/copy).
2. WP-30 one-off copies: confirm a green CI run with ca88da4f/69b162f3; close the low items.
3. WP-52 importer hand-over. DONE (c8da93ca adds reverse 14.2, per-person gate, 7.12 item 6). Earlier: (aviary 430539e4, holds.js 19b90a2e, router header claudecodeui e9df2733): the loop, gate, upload ingestion, export-server ingest route, allow-handover, tests/loop.rs (7.12 item 5). Open: reverse (14.2) with a reverse fence in magpie; per-person gate; 7.12 item 6 fence cases (fence-race fuzz, leader kill -9 between fence and release, magpie switchover in a fence).
4. WP-28 integration harness + release pipeline: E20, E22, nightly e2e-chaos, image push after the harness,
   release aviary-<sha12> with static binaries + SHA256SUMS, protocol labels, aviary-testing image, jarvis3 chat.yml
   conformance on the image, pigeon-frames/paseo-contract release assets, the real Claude Code 2.1.304 e2e of ccfs.
5. WP-35 cutover runner + patches, incl. WP-27's freeze-check.
6. WP-54 switch verification rigs.
7. The Talos cluster: T1 DONE 2026-11-04 (YYC 6e479208, e3928f6b, ed12033a): 3 x cax11 nbg1 control nodes, Cilium,
   KubeSpan, WARP, hccm, CSI, Sealed Secrets, CNPG, cert-manager, OIDC authn, etcd backups hourly to yyc-talos-etcd
   (tested), Descheduler, node-work RBAC; access through ~/artifacts/on-start/talos-tunnel.sh. Waiting: Deyao's A/B
   for window workers (Discord 2026-11-04); Grafana stack for Alloy (a new account: ask); deploy connector (tunnel
   yyc-deploy, WP-33); sandboxd rule priority 100 clashes with Talos's local rule (WP-19 owner picks another).
8. WP-31 YYC deploy layout, WP-32 rest (secrets.txt, seal.py by folder, manifests), WP-33 Cloudflare/cross-namespace,
   WP-34 fabric/AMI, WP-36 operator tooling and docs (yyc-ai skill rewrite).
9. Operator phase WP-40..46 (bridges: the server yyc-bridge1, EUR 7.188/month gross during the window, inside the
   approved EUR 16.78 of the two load balancers Hetzner refused).

## Progress 2026-11-04 (afternoon)
- DONE: WP-52 (c8da93ca), WP-35 (YYC ea1f2f44, 7a965aa2), WP-31/32/33/36 (YYC 42bea902..bdf5cf8e), WP-37 (3007f1f8),
  WP-60 code (d3d95fa7; LBs not created: wait until new workers exist), WP-39 LIVE (Flux reads yyc-ai/legacy, prune on,
  pods unchanged; yyc-ai/next promoted to yyc-ai/deploy), old servers labelled yyc-cluster=k3s, cloudflare custom
  hostname drift fixed, pigeon LU-drop write fix (aviary c425c1ad), Talos T1 + WP-59 pieces live.
- RUNNING (agents): WP-28, WP-54 rest (legacy-in-a-box, personas), WP-61/62.
- WAITING on Deyao: window workers A/B; Grafana stack for logs; rotate the discovery secret?
- NEXT for me: workers (after A/B) -> JuiceFS CSI, socks pool and gateway node on the new cluster; bridges up; seal the
  new-cluster Secrets (needs the session's next secret pull for keys written today); WP-35 patches regenerated against
  the real tree; sandboxd ip-rule priority vs Talos's 100; then A1.
- 2026-11-04 evening: WP-61/62 done (aviary f7e4a551, jarvis3 156c50b, YYC ccc61968); new stack pinned to
  aviary-cbe7ea90ca6b (YYC cba0f7ec); aviary releases fixed (deploy-key tag push, cbe7ea90); WP-54 complete and green
  on a fresh legacy-in-a-box run (aviary 9a37524f, claudecodeui cdbfe9fa) after fixes: asks on moved tabs (pigeon
  4be025a1), importer first-turn release (380a38bb), live verify-mismatch counter (51916cee, 21b43b5a), E5 runner gate
  (YYC 377d57db); supervisor watchdog-kill race fixed (3a0bf915). Waiting on Deyao: workers A/B, A1 upload window
  (a/b/c), Grafana stack, discovery-secret rotation.
- 2026-11-05: workers option B taken on Deyao's "continue" (3 x cax21 nbg1, w1-w3 at 10.0.0.31-33, placement group
  yyc-talos-workers created, WARP keys YYC_TALOS_WARP_W1..W3 stored, configs rendered). jarvis3-chat (jarvis3 92aa7c6)
  and pigeon-app (claudecodeui 4ad45052) now build arm64 too. Server create refused: Hetzner "Primary IP limit
  exceeded" (14 in use account-wide). Waiting on Deyao: OK to file Hetzner's limit-increase request.
- 2026-11-05 later: no limit raise (Deyao). Workers yyc-talos-w1..3 (cax21 nbg1, 10.0.0.31-33) run with NO public IP;
  network yyc's route 0.0.0.0/0 -> yyc-nat1 (10.0.0.30, Debian cax11, WARP + masquerade + dnsmasq; talos/nat/nat.sh).
  All 6 nodes Ready, KubeSpan peers up, pod egress via WARP and ghcr reachable from w1. aviary-build deleted after
  snapshot 452253498 (default project). Worker WARP keys YYC_TALOS_WARP_W1..W3 stored but unused.
  NEXT: JuiceFS CSI, socks pool, gateway node, bridges, seal secrets, WP-35 patches, then A1.
- 2026-11-05 gateway: yyc-pool-small-worker5 drained, removed, snapshot 452284716, recreated on its primary IPv6 as
  Talos yyc-talos-gw1 (x86 cx23 fsn1, 10.0.0.7, firewall yyc-talos-gateway); gateway-net.sh (aviary) set route
  10.202.0.0/15 -> 10.0.0.7 and the label. yyc-nat1 moved to fsn1 (exit PRG 104.28.207.215). Old small pool now 4.
  Agents running: aviary rule priority (+re-pin), new-cluster plumbing (N1/N9/N6/socks/sealing), Postgres HA + moves plan.
- 2026-11-05 bridges: Hetzner refused the two lb11 (primary IP limit); WP-60 is now ONE server `yyc-bridge1` (id
  174553919, cax11 fsn1, Debian 13, no public IP, 10.0.0.12, alias IPs 10.0.255.1/.2, HAProxy TCP; EUR 7.188/month
  gross), `bridges.sh up` live and idempotent. `bridges.sh check` red only for missing callees: the old cluster's
  NodePorts 30880-30882 (a1-2/a1-3 patches, A1) and the new stack's pigeon/hermit (30890/30893). A source-address
  probe from 10.0.255.1/.2 reaches both clusters' nodes.
- 2026-11-05 afternoon: deploy path live (N1/N9, talos-cluster.yml on push as gha:talos-cluster; authenticator also
  admits yyc-apps.yml), JuiceFS CSI, socks pool on w1-w3 (IPv4 via yyc-nat1), new-stack Secrets sealed except
  hermit/tunnel/magpie-import/gateway; bridges = yyc-bridge1 HAProxy (no public IP, 10.0.255.1/.2, EUR 7.188 gross;
  LBs impossible under the primary IP limit); magpie-pg -> nbg1; gateway pool=gateway; talos.sh roles news/db;
  fabric PKI + gateway WG key + YYC_HERMIT_JUICEFS_META_URL in store; Postgres HA plan (EUR 19.11 with magpie-pg)
  and app moves plan: yyc-ai/refactor/talos-apps-move.md. talos-ready: only N10 fails (surge slots, timing).
  Running: aviary rule priority + re-pin; WP-34 (AWS IAM/SG/AMI workflow, runbook, sealing hermit + gateway).
  NEXT: WP-35 patches remade after the re-pin; then A1 (operator phase, 04:00 HKT quiet hour).
- 2026-11-05: another session (s2023a296073179ae) is patching the old stack's out-of-memory (CloudCLI per-person cap
  1 GB, bigger jarvis/ui pods, cpx32 added/resized on the old cluster). Deyao: the move fixes the root cause, so those
  servers are released after A6: talos-apps-move.md "A6+: release the 2026-11-05 emergency capacity".
- 2026-11-05 06:30 UTC: service session done (large-worker1 cpx42 in place, large-worker2's WARP fixed, all 9 nodes Ready).
  Its "conversation too long" message goes on claudecodeui main and ships in A1's ui rollout: 08 and pigeon-app re-pinned
  to that main commit at A1 (if service needs it sooner, it ships from a branch off the live image first). A1 continues.
- 2026-11-05 08:14 UTC: **A1 done.** Steps 1-7 on both clusters (pg roles/dbs, magpie-pg 3/3 + backup, B3, bridges all
  green on yyc-bridge1, tofu pigeon tunnel, new stack deployed dark on yyc-talos: pigeon-test 200, hermit-gateway on gw1),
  step 8 the gated ui rollout to claudecodeui fd1b3ed5 (conversation-too-long line, view-as files, DECISIONS 17 parked
  questions): 08 at 08:0x, gate 08:03-08:14, 09, a1_rollout_cut_total 0. Freeze armed (window.json).
  NEXT: A2 (start-handover) after the new-stack agent's importer carry-over of parked questions (DECISIONS 17) lands
  and is re-pinned; T4a (mail merge, AI watch) per talos-apps-move.md, outside A3-A5.
- 2026-11-05 P2 done: small-worker1-3 -> yyc-talos-news1-3 (x86 cx23 fsn1, pool=news, kept IPv6 + 10.0.0.5/8/9;
  snapshots 452361048 and two more; talos/rebuild-old.sh). Old cluster: 6 nodes; its hcloud CCM pinned off the master.
  The new-cluster readiness for T4a is there (x86 room 3 x ~2.67 Gi). NEXT: T4a (mail merge, AI watch); A2 after
  the importer carry-over.
