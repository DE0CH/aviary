# testing/legacy-in-a-box: the switch window on one machine (WP-54)

Both stacks as decision 13 has them, in two k3d clusters on one Docker network, with the switch driven through a
real browser. Nightly and manual only (`nightly.yml` job `legacy-in-a-box`); it gates nothing until the whole rig
passes.

```
testing/legacy-in-a-box/run.sh up <release sha12> <ui image>      # both clusters, the bridges, the old Postgres
testing/legacy-in-a-box/run.sh smoke                               # an old turn through B1, then the bulk import
testing/legacy-in-a-box/run.sh seeds | reset                       # a seeded defect per zero counter; fresh processes
testing/legacy-in-a-box/run.sh personas <claudecodeui> <persona build>
testing/legacy-in-a-box/run.sh a1-rollout [start|result]          # the A1 gated ui rollout stand-in (persona a1)
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

`run.sh seeds` (before the gate: the upload tee lasts from T0 to it) proves the counter path: a seeded defect per zero
counter, each of which must move its counter so that `counters --check` fails naming it:

| Counter | Seeded defect |
|---|---|
| `switch_inflight_closes_total` | a client that sends a request after the move's close frame (`seed-inflight.mjs inflight`) |
| `switch_opaque_sockets` (`totals.opaque`) | a client that sends a fragmented message (`seed-inflight.mjs opaque`), held open |
| `pigeon_lu_401_total` | Access publishes its next key (fake-access `k2`) and the old router's JWKS file lacks it: a tab signed with it opens a legacy conversation |
| `pigeon_lu_503_total` | the bridge's B1 answers 503 (a full old pod): an LU upgrade |
| `pigeon_upload_tee_failed_total` | B1's target gone (`bridge.mjs` fault `refuse`): an upload teed into the old child |
| `pigeon_switch_write_refused_total` | the same: a send to a legacy conversation with no LU |
| `pigeon_lu_errors_total` | the same: an LU call |
| `magpie_conversation_migrating_total` | a native write straight to magpie for a legacy conversation (an owner record gone wrong) |
| `magpie_unknown_upload_total{legacy="true"}` | a create whose attachment is an old asset path with no file anywhere |
| `magpie_import_verify_mismatch_total` | the old side writes a released conversation: a person's own gate releases it, then a line is appended to its transcript on the shared mount (the hand-over loop's live released check counts it) |
| `old_turn_cut_total` | an old pod drained (5 s deadline) under a long turn |

(`seed-pigeon.mjs` is the seeds' plain client of pigeon: a Paseo socket and the upload route.) `run.sh reset` then
rolls every counting process (pigeon, magpie, the hand-over, the old ui pods) so the personas are checked at 0 on
their own.

`personas` runs the persona drivers, each as a person of its own on a fresh legacy conversation (DESIGN 9.9):
`draft` twice with a seeded defect (`lost-draft` must fail "draft kept across the move", the edge's
`second-reconnect` must fail "one reconnect per move"), then `a1` (the A1 gated ui rollout before T0, DESIGN 9.6 and
DECISIONS E5: four tabs open on the old ui and a long old turn running when `run.sh a1-rollout` pushes the rollout,
the Deployment paused with a new template, `BOX_A1_IMAGE` or the same image, as Flux applies `08-ui-rollout`; the
gate stand-in resumes only while no old pod has running work, read through every child's switch busy route as
cutover's `ui-gate` reads it, for `BOX_A1_QUIET` s, and pauses again before an old pod's SIGTERM when work appears;
the turn is not cut and the gate waited for it, `a1_rollout_cut_total` is 0, every tab reconnects at most once and
is connected after), `inflight` (a send in flight at the move), `upload` (an
upload after T0 on an unmoved tab, sent on the old socket), `attach` (a file attached at move - 2 s, sent at move + 3
s: a pigeon-born conversation), `ask` (an AskUserQuestion across the move, Stop from the moved tab), `view-as` (an
admin's view-as tab), `bridge` (B1's connections reset mid-send: every write lands once, no reconnect), `hold` and
`warm` (a hold and an idle warm old sandbox, with their person's own gate: released only after), and last the clean
`draft`, which goes on through control.mjs to the gate for everyone (`run.sh gate`: `magpie import allow-handover`,
which reads every ui pod's switch report through the old API), the release while the tab shows the conversation (no
reconnect, the composer untouched by the replacement) and a native next send. Then `counters --check`: every zero
counter of `testing/switch/zero-counters.txt` is 0, summed over pigeon, magpie and the importer (the API's pod
proxy), each old ui pod's report and the old ui log. `BOX_ONLY="<persona>…"` runs just those, by hand.

Not done yet: iOS.
