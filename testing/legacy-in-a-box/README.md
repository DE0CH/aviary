# testing/legacy-in-a-box: the switch window on one machine (WP-54)

Both stacks as decision 13 has them, in two k3d clusters on one Docker network, with the switch driven through a
real browser. Nightly and manual only (`nightly.yml` job `legacy-in-a-box`); it gates nothing until the whole rig
passes.

```
testing/legacy-in-a-box/run.sh up <release sha12> <ui image>      # both clusters, the bridges, the old Postgres
testing/legacy-in-a-box/run.sh smoke                               # an old turn through B1, then the bulk import
testing/legacy-in-a-box/run.sh personas <claudecodeui> <persona build>
testing/legacy-in-a-box/run.sh logs <dir> | down
```

Needs what `testing/e2e` needs (docker, k3d, kubectl, jq, openssl, juicefs, sudo; `crane` with `E2E_PRELOAD=1`) and
node 22. `E2E_PG_PORT`/`E2E_S3_PORT` move the harness's host ports on a machine whose own services hold them;
`BOX_KEEP_NEW=1` reruns only the old side.

| Piece | Where |
|---|---|
| the new stack | `testing/e2e` unchanged (k3d `aviary-e2e`), plus `k8s/new.yaml`: Service `ui-legacy` (B1's front, what pigeon's LU calls), NodePort `pigeon-bridge` (B2's target), the importer's Secret, the old API's CA and the hand-over Deployment, NetworkPolicies for exactly these |
| the old stack | k3d `aviary-old`, namespace `jarvis` (`k8s/old.yaml`): two ui pods of the A1 image (peers, the switch module on, the purge off), the web role and sandbox agent stand-in (`testing/lu-rig/old/fake-web.mjs`, which also writes `jarvis.sandboxes`), the mock model; Postgres `aviary-old-pg` (`cloudcli`, `jarvis`) beside it; `/jfs` is the harness's JuiceFS mount in both clusters |
| bridges | `bridge.mjs` (`aviary-bridge`): B1 `:8080` -> the old ui's NodePort, B2 `:8081` -> pigeon's; `POST :9999/__bridge/cut` resets every connection. B3 (the old Postgres) and B7 (the old API, a ServiceAccount token) are reached on the network |
| edge | `edge.mjs` (`aviary-edge`): the browser's origin; serves the persona build, passes the rest to the old ui until `/__edge/flip`, then to pigeon except a stale share; `/__edge/defect?name=second-reconnect` |
| personas | DE0CH/claudecodeui `jarvis/test/persona/personas.mjs` (Playwright, Chromium) on the persona build (`build.sh`: `EXPO_PUBLIC_YYC_PERSONA=1`, `window.__yycPersona`, the `lost-draft` defect) |

`personas` runs the `draft` persona three times on a fresh legacy conversation each: clean (must pass: one reconnect
at the move, the draft kept, the send after the move accepted once, by the old child through pigeon's LU), with
`lost-draft` (must fail "draft kept across the move") and with the edge's `second-reconnect` (must fail "one
reconnect per move").

Not done yet: the gate and the hand-over end to end (the importer's Deployment needs a release with WP-52's
hand-over; a release then a replacement while a tab shows the conversation), the other personas of DESIGN 9.9 (a
send in flight at the move, an upload after T0 on an unmoved tab, a file attached at move - 2 s to a pigeon-born
conversation, an ask, view-as, a hold, an idle warm old sandbox, the A1 gated rollout, a bridge-node kill), iOS, and a
seeded defect per zero counter.
