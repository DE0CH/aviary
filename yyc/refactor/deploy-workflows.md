# YYC AI deploys through GitHub workflows (decision 1)

Status: design, 2026-11-02, for Deyao's decision 1 ("a push to DE0CH/YYC applies yyc-ai through a workflow; the Flux
Kustomization yyc-ai is removed"). It replaces spec-platform 13.1, 13.2's reconcile step and 13.4, and every place in
spec-platform and spec-cutover that says "Flux applies / reconciles / prunes" for `yyc/deploy` (section 10 lists
them). claude-env CLAUDE.md "GitOps runs in GitHub workflows" and "Deploy means zero downtime" are the rules it
follows. Writing only: nothing here is applied yet.

**Revision 2026-11-03 (decision 13, DESIGN 15).** The new stack lands on the new Talos cluster and the old stack stays
on the old k3s cluster, so `yyc-ai.yml` targets the **new** cluster and there is **no Flux hand-over**: the old
cluster keeps Flux (Kustomizations `yyc-ai` and `yyc-ai-secrets`, now reading `yyc-ai/legacy/`) until the Talos
session removes it. Section 12 is the text of record for what that changes: the API path and authenticator on Talos
(12.2), the RBAC on the new cluster (12.3), what goes from the implemented applier and workflow (12.4), the bring-up
order that replaces section 8's H0-H4 (12.5) and the edits to the other documents (12.6). Sections 3, 4 and 8 below
are rewritten for it; where any other text here still says the deploy reaches `yyc-master1`, applies `jarvis` or
hands over from Flux, section 12 holds.

Contents
1. What changes, in one page
2. Facts it rests on (read from the repo and the live cluster, 2026-11-02)
3. The path from a GitHub runner to the API server
4. Who the workflow is in the cluster (identity and RBAC)
5. The workflow `yyc-ai.yml`
6. The applier `yyc-apply` (render, plan, gate, apply, wait, prune, inventory)
7. Credentials: where each lives and how it reaches GitHub without being printed
8. The hand-over from Flux without deleting a live object
9. Failure modes
10. Changes to spec-platform.md, spec-cutover.md, DESIGN.md and WORK-PACKAGES.md
11. What stays on Flux, and why
12. Decision 13: the new cluster is the target

---

## 1. What changes, in one page

- **One workflow, `DE0CH/YYC/.github/workflows/yyc-ai.yml`.** Job `check` (every PR and push that touches YYC AI:
  today's planned `yyc-ai-check` and `yyc-ai-render`, plus the ZDR guard as a gate) and job `deploy` (push to `main`,
  every 3 hours against drift, and `workflow_dispatch`), which needs `check`. `deploy` renders `yyc/deploy`, prints
  the plan against the live state, refuses deletions the commits did not name, applies with server-side apply in a
  fixed order, waits for every rollout to finish, prunes, and records what it applied. The run log and a green or red
  check sit on the commit; a red run (and the first green after it) goes to YYC's Discord channel, as `cloudflare.yml`
  does.
- **The runner reaches the API through Cloudflare.** The target is the new Talos cluster (decision 13). Its nodes are
  IPv6-only, while GitHub's hosted runners are IPv4-only. A new tunnel `yyc-deploy` (connector in the new cluster,
  namespace `yyc-deploy`) publishes the API as TCP behind an Access app that admits one service token; the runner uses
  `cloudflared access tcp` to `127.0.0.1:6443`.
- **No cluster credential in GitHub.** The API server trusts GitHub Actions' OIDC tokens through a structured
  authentication config (`AuthenticationConfiguration`, GA; on Talos in the control nodes' machine config, 12.2),
  accepting only tokens from `DE0CH/YYC`, ref `refs/heads/main`,
  environment `yyc-ai`, workflow `yyc-ai.yml`. The workflow mints a fresh token per kubectl call. GitHub holds only the
  Access service token, which reaches the API port and grants nothing inside it.
- **Least privilege.** The identity `gha:yyc-ai` may manage the kinds the render contains in `yyc-ai` and
  `yyc-ai-fabric`, a named handful of cluster-scoped and cross-namespace objects, and its own inventory ConfigMap
  (12.3). It cannot read Secrets, create namespaces, touch RBAC outside those namespaces or change Pod Security labels.
  Sealed Secrets keep their fence: the workflow applies `sealed/` impersonating `yyc-deploy/sealed-secrets-applier`,
  which may only manage SealedSecret objects.
- **Namespaces and the deployer's own RBAC are cluster-level objects** in the new cluster's cluster-level folder
  (`news-stream/deploy/clusters/talos/`, applied by the new cluster's own workflow, not by `yyc-ai.yml`), so a yyc-ai
  commit can neither widen its own rights nor relabel a namespace privileged. The JuiceFS CSI driver is a cluster
  add-on there too, like CNPG and Sealed Secrets.
- **Deletions need the commit's say-so.** Anything the render no longer contains is deleted only if a commit since the
  last applied one names it in a `Deploy-Prune:` trailer (hashed generator ConfigMaps excepted); a namespace needs
  `Deploy-Prune-Namespace:`. A commit that drops an object by accident turns the check red and changes nothing. The
  same applier runs the plan from a session against the live state before such a push.
- **No hand-over from Flux** (decision 13). The new cluster starts empty, so the workflow's first run creates the new
  stack and its inventory starts empty; the old stack stays on the old cluster's Flux (from `yyc-ai/legacy/`) until
  that cluster is removed (12.1, 8).
- **Time and cost.** About 4 Claude days (applier, workflow, tunnel, auth config, docs; the hand-over part is dropped by decision 13 and the retargeting is about 1.5 days, 12.4). The tunnel and
  Access app cost USD 0.00 (Zero Trust Free); the connector adds 2 × 64 Mi on the apps nodes. DE0CH/YYC is private,
  so runs draw on the account's included Actions minutes, which `cloudflare.yml`'s 30-minute schedule already uses;
  the drift schedule here is every 3 hours and a schedule run skips `check`, so the added draw is about
  8 runs/day plus one per push, each billed per job rounded up to the minute. The exact minutes per run are measured
  after H3 and compared with the plan's included minutes from GitHub's billing API before P1 (9, last row).

---

## 2. Facts it rests on (read from the repo and the live cluster, 2026-11-02)

| Fact | Source | Consequence |
|---|---|---|
| No YYC or DE0CH/deyao-ai k8s folder is applied by a workflow yet; `cloudflare.yml` (tofu) is the only workflow-applied infra in DE0CH/YYC, with the plan/apply/Discord pattern this design copies | `YYC/.github/workflows/cloudflare.yml`; skills `yyc-news-stream`, `transcript-search` (Flux) | this is the first kubectl-applying workflow; the applier is written for reuse by the other folders later (11) |
| API server only from the private network 10.0.0.0/16; every server IPv6-only; outgoing IPv4 through WARP | `news-stream/deploy/cluster/README.md` lines 3-7, `hetzner-k3s.yaml` `allowed_networks.api` | a GitHub-hosted runner (IPv4 only, no route into the private network) needs a relay: section 3 |
| `kubernetes` endpoint = 10.0.0.3:6443 (the master's private IP) | `yyc/deploy/ops.yaml` (`web-to-apiserver`) | the connector's egress NetworkPolicy names exactly that address and port |
| k3s v1.36.5+k3s1, one control-plane node | `kubectl version`; cluster README | `AuthenticationConfiguration` (structured JWT authenticators with CEL rules) is GA and reloads the file without a restart; enabling it once restarts k3s on `yyc-master1` |
| Flux v2.9.6, kustomize-controller v1.9.6; Kustomization `yyc-ai` prune true, `yyc-ai-secrets` fenced (`serviceAccountName: sealed-secrets-applier`), both Ready at `main@c0d8e5c8` | `flux get kustomizations`; `clusters/hetzner/flux-sync.yaml` | kustomize-controller ≥ v1.4 has `spec.deletionPolicy` (`Orphan` leaves every inventory object on deletion); `flux-sync.yaml` is applied by hand (`cluster.sh flux`), not by Flux |
| `yyc-ai`'s inventory: 46 objects, among them Namespace `jarvis`, PV `yyc-ai-jfs`, RuntimeClass `gvisor`, `kube-system/sbx-overlay-route`, `yyc/yyc-catalogue`, HelmRelease `flux-system/juicefs-csi-driver`, HelmRepository `flux-system/juicefs`, RoleBinding `jarvis/sealed-secrets-applier`, hashed ConfigMap `jarvis-config-tdc5d62c24` | `kubectl get kustomization yyc-ai -o jsonpath={.status.inventory}` | the seed inventory and the RBAC of section 4 cover exactly these plus the new stack |
| Objects carry field manager `kustomize-controller` (Apply) and labels `kustomize.toolkit.fluxcd.io/{name,namespace}` | `kubectl get deploy web -o jsonpath={.metadata.managedFields}` | the hand-over transfers field ownership (8, step H3.c), or later field removals would silently stay |
| Repo secrets are set with `gh secret set` and `YYC_CI_SECRETS_TOKEN` | `news-stream/deploy/README.md` "Cloudflare (tofu)" | the same path for the two new environment secrets |
| A resealed Secret once rolled Pods before the controller decrypted it ("bumped twice") | `yyc/deploy/kustomization.yaml` secrets-rev comment | the applier waits for each changed SealedSecret's `Synced` condition before it applies workloads (6.4) |

---

## 3. The path from a GitHub runner to the API server

```
GitHub-hosted runner (IPv4)                     Cloudflare                       new (Talos) cluster
cloudflared access tcp  ──wss + service token──► Access app "YYC deploy API"  ──► tunnel yyc-deploy
  127.0.0.1:6443                                 (k8s-deploy.yyc-internal.com,     connector: Deployment
kubectl ──TLS (cluster CA)──────────────────────────────────────────────────────►  yyc-deploy/cloudflared (2)
                                                 TLS is end to end: Cloudflare      └► tcp://kubernetes.default
                                                 carries bytes, never sees the         .svc.cluster.local:443
                                                 bearer token                          = the API servers (N1) 
```

**Tofu** (`news-stream/deploy/cloudflare/tofu/deploy.tf`, applied by `cloudflare.yml`):

| Resource | Values |
|---|---|
| `cloudflare_zero_trust_tunnel_cloudflared.yyc_deploy` | name `yyc-deploy`, `config_src = "cloudflare"` |
| `cloudflare_zero_trust_tunnel_cloudflared_config.yyc_deploy` | ingress `k8s-deploy.yyc-internal.com` → `tcp://kubernetes.default.svc.cluster.local:443`; catch-all `http_status:404` |
| `cloudflare_dns_record.k8s_deploy` | CNAME `k8s-deploy` → `<tunnel id>.cfargotunnel.com`, proxied |
| `cloudflare_zero_trust_access_service_token.yyc_deploy_gha` | name `yyc-deploy (GitHub Actions)`, `duration = "forever"`: it only opens the TCP path; authentication inside is the OIDC token (4.1), so rotation is on demand, not on a calendar |
| `cloudflare_zero_trust_access_policy.yyc_deploy_gha` | decision `non_identity`, include that service token only |
| `cloudflare_zero_trust_access_application.yyc_deploy` | `self_hosted`, domain `k8s-deploy.yyc-internal.com`, the policy above only, session duration 1 h |
| outputs (sensitive) | `yyc_deploy_tunnel_token`, `yyc_deploy_access_client_id`, `yyc_deploy_access_client_secret` |

A separate tunnel, not an ingress rule on `yyc-stream` or the YYC AI tunnels: its token sits only in namespace
`yyc-deploy`, and a mistake in an app's tunnel config cannot open the API.

**Connector** (`news-stream/deploy/clusters/talos/deploy-access.yaml`, applied by the new cluster's workflow): Namespace
`yyc-deploy` (PSS restricted); Deployment `cloudflared` (2 replicas, `maxSurge 1 maxUnavailable 0`, PDB
`minAvailable 1`, hostname spread, priority `serving`, 64 Mi request = limit, `tunnel --no-autoupdate run` with
`TUNNEL_TOKEN` from SealedSecret `yyc-deploy/tunnel`, readiness on `:2000/ready`); NetworkPolicies: default deny
ingress; egress only to the new API's endpoints (each /32 on its port, DESIGN 15.2 N1), Cloudflare's edge on 7844/TCP+UDP and 443/TCP, and DNS. The fenced
applier gets a RoleBinding in `yyc-deploy` (the tunnel Secret is applied by `yyc-apps-secrets`, as every other
cluster-level Secret).

**Runner side**: `cloudflared access tcp --hostname k8s-deploy.yyc-internal.com --url 127.0.0.1:6443`, with
`TUNNEL_SERVICE_TOKEN_ID` / `TUNNEL_SERVICE_TOKEN_SECRET` in its environment (never on the command line, so no
process listing shows them; the flag/env names are checked against the pinned cloudflared release when built). The
kubeconfig points at `https://127.0.0.1:6443`, which is in the API server certificate's SANs (N1), with
`certificate-authority: infra/yyc-talos-ca.pem` (the new cluster's Kubernetes CA, public; committed, and on the cluster README's
"after a rebuild" list).

---

## 4. Who the workflow is in the cluster (identity and RBAC)

### 4.1 Authentication: GitHub OIDC, no stored credential

On the new cluster's three control nodes, in their Talos machine config (the builder's, handed over before T1 as
DESIGN 15.2.1 input I4): the file below, supplied by this patch. Talos has no field for a structured authentication
file that this design relies on; `machine.files` may write only under `/var`, so the file goes there and is mounted
into the API server's static pod. WP-59 checks the patch against the Talos release the builder installs
(`talosctl validate --mode cloud` on the merged config, and that release's machine-config reference); if that release
has a native field, the builder may use it with the same file content.

```yaml
machine:
  files:
    - path: /var/etc/kubernetes/yyc-authn/authentication-config.yaml
      permissions: 0o644
      op: create
      content: |
        # the AuthenticationConfiguration below, verbatim
cluster:
  apiServer:
    certSANs: [127.0.0.1]
    extraArgs:
      authentication-config: /etc/kubernetes/yyc-authn/authentication-config.yaml
    extraVolumes:
      - hostPath: /var/etc/kubernetes/yyc-authn
        mountPath: /etc/kubernetes/yyc-authn
        readonly: true
```

The API server refuses `--oidc-*` flags beside `authentication-config`, so the builder's config sets none. The file:

```yaml
apiVersion: apiserver.config.k8s.io/v1
kind: AuthenticationConfiguration
jwt:
  - issuer:
      url: https://token.actions.githubusercontent.com
      audiences: [yyc-cluster]
    claimValidationRules:
      - {expression: "claims.repository_id == '<DE0CH/YYC numeric id>'", message: "repo"}
      - {expression: "claims.ref == 'refs/heads/main'", message: "main only"}
      - {expression: "claims.environment == 'yyc-ai'", message: "environment"}
      - {expression: "claims.job_workflow_ref.startsWith('DE0CH/YYC/.github/workflows/yyc-ai.yml@refs/heads/main')", message: "workflow"}
    claimMappings:
      username: {expression: "'gha:' + claims.environment"}      # gha:yyc-ai
      groups:   {expression: "[]"}
```

The numeric `repository_id` survives a rename and cannot be claimed by a new repo of the same name. The API server
fetches GitHub's JWKS over IPv4 through each control node's WARP link (`token.actions.githubusercontent.com` has no
AAAA record, `dig`, 2026-11-03). DESIGN 15.2 N1 checks each control node separately (a GitHub token sent to each
node's own address). With WARP down on one control node, that API server cannot refresh the keys: once GitHub signs
with a key it has not cached, it rejects the token, so about one deploy request in three fails (the run is red and is
re-run); workloads, the other two API servers and the operator path (client certificates, N2) are unaffected. Applying the config restarts one
API server at a time (the other two serve, so the API never pauses). Later edits of the file (another environment
for a future folder or for the Talos session's own workflows) are reloaded by the API server without a restart.

Considered and not taken: a ServiceAccount token in GitHub secrets (a long-lived cluster credential outside the
cluster, the concern of DESIGN 14 Q1); a self-hosted runner inside the cluster (keeps credentials in, but the runner
then deploys the cluster it runs in, needs a GitHub registration credential in the cluster and memory the apps nodes
lack).

### 4.2 Authorisation (all in `clusters/talos/yyc-ai-access.yaml`, applied by the new cluster's workflow)

| Object | Grants `gha:yyc-ai` |
|---|---|
| Namespaces `yyc-ai` (PSS restricted), `yyc-ai-fabric` (PSS privileged), `yyc-deploy` (PSS restricted) on the new cluster | — (the deployer cannot create, relabel or delete a namespace) |
| ClusterRole `yyc-ai-deployer-ns`, bound by a RoleBinding in `yyc-ai` and `yyc-ai-fabric` | create, get, list, watch, patch, update, delete on: `configmaps`, `services`, `serviceaccounts`, `persistentvolumeclaims`, `limitranges`, `apps/{deployments,daemonsets,statefulsets}`, `batch/{jobs,cronjobs}`, `autoscaling/horizontalpodautoscalers`, `policy/poddisruptionbudgets`, `networking.k8s.io/networkpolicies`, `discovery.k8s.io/endpointslices` (the bridge Services' slices, DESIGN 15.5), `rbac.authorization.k8s.io/{roles,rolebindings}`; get, list, watch on `pods`, `apps/replicasets`, `events`, `bitnami.com/sealedsecrets` (to read their `Synced` status). **No `secrets` verb at all.** |
| ClusterRole `yyc-ai-deployer-cluster` + ClusterRoleBinding | `persistentvolumes`: create, and get/patch/delete with `resourceNames [hermit-jfs]` |
| Role in `yyc` | `services` and `discovery.k8s.io/endpointslices` create, and get/patch/delete `[yyc-mcp, web]` (the bridge placeholders B5, B6, until T4 moves those apps); after yyc-mcp's move, `configmaps` create and get/patch `[yyc-catalogue]` (DESIGN 15.4) |
| Role in `yyc-deploy` | `serviceaccounts` impersonate `[sealed-secrets-applier]`; `configmaps` create, and get/patch/update `[inventory-yyc-ai]` |
| RoleBindings of ClusterRole `sealed-secrets-applier` (the fence) in `yyc-ai`, `yyc-ai-fabric`, `yyc-deploy` | (to `yyc-deploy/sealed-secrets-applier`, DESIGN 15.2 N9) |

Nothing on the old cluster grants `gha:yyc-ai` anything: the old cluster has no authenticator for it.

Roles the render creates (hermit's `hermit-read`) only hold
permissions the deployer itself holds, so Kubernetes' escalation check passes with no `escalate` or `bind` verb.

What this identity can still do, stated plainly: it can run privileged Pods in `yyc-ai-fabric` (the gateway
DaemonSet is privileged by design), so it is root on the nodes, as Flux's kustomize-controller (cluster-admin) was.
Only a workflow run of `yyc-ai.yml` on `main` in environment `yyc-ai` can obtain it, which is the trust a push to
`main` already had under Flux.

`yyc-ai-check` keeps the render and this table in step: a rendered (namespace, kind, name) outside the table fails the
check with "extend clusters/talos/yyc-ai-access.yaml", so a new kind is a cluster-level commit first.

---

## 5. The workflow `yyc-ai.yml`

```yaml
name: yyc-ai
# YYC AI's deploy (claude-env CLAUDE.md "GitOps runs in GitHub workflows"; docs: yyc-ai/README.md "Deploys").
# check: every PR and push touching YYC AI. deploy: push to main, every 3 h (reverts drift), dispatch.
# The runner reaches the API through tunnel yyc-deploy (Access service token) and authenticates with its own
# GitHub OIDC token (the API server's authn.yaml): no cluster credential is stored anywhere in GitHub.
on:
  push:
    branches: [main]
    paths: &p
      - yyc/deploy/**
      - yyc/tests/test_zdr.py
      - infra/secrets.txt
      - infra/yyc-talos-ca.pem
      - yyc-ai/legacy/**                         # check only (freeze-check, ZDR guard's old half); never deployed
      - tools/yyc-apply/**
      - .github/workflows/yyc-ai.yml
  pull_request: {paths: *p}
  schedule: [{cron: "7 */3 * * *"}]
  workflow_dispatch:
    inputs:
      mode: {type: choice, options: [apply, plan], default: apply}
concurrency: {group: yyc-ai-deploy, cancel-in-progress: false}
permissions: {contents: read}
jobs:
  check:
    if: github.event_name != 'schedule'            # a schedule run re-applies main, already checked on its push
    runs-on: ubuntu-24.04
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v4
        with: {fetch-depth: 0}
      - uses: ./.github/actions/k8s-tools          # kustomize 5.8.1, kubeconform 0.8.0, crane, kubectl 1.36.5,
                                                   # cloudflared <pin>; each by version + sha256
      - uses: Swatinem/rust-cache@v2
        with: {workspaces: tools/yyc-apply}
      - run: cargo build --release --locked --manifest-path tools/yyc-apply/Cargo.toml
      - run: tools/yyc-apply/target/release/yyc-apply check --folder yyc/deploy     # spec-platform 13.3 + 6.6
      - run: python3 yyc/tests/test_zdr.py
        env: {GITHUB_TOKEN: "${{ github.token }}"}
  deploy:
    needs: check
    if: >-
      always() && github.ref == 'refs/heads/main' && github.event_name != 'pull_request' &&
      (needs.check.result == 'success' || (github.event_name == 'schedule' && needs.check.result == 'skipped'))
    runs-on: ubuntu-24.04
    timeout-minutes: 45
    environment: yyc-ai                             # deployment branches: main only; holds the Access secrets
    permissions: {contents: read, id-token: write, actions: read}
    steps:
      - uses: actions/checkout@v4
        with: {fetch-depth: 0}                      # trailers since the last applied commit
      - uses: ./.github/actions/k8s-tools
      - uses: ./.github/actions/yyc-apply           # the built binary from actions/cache keyed by the hash of
                                                   # tools/yyc-apply/**; builds only on a miss
      - name: connect
        env:
          TUNNEL_SERVICE_TOKEN_ID: ${{ secrets.YYC_DEPLOY_ACCESS_CLIENT_ID }}
          TUNNEL_SERVICE_TOKEN_SECRET: ${{ secrets.YYC_DEPLOY_ACCESS_CLIENT_SECRET }}
        run: tools/yyc-apply/connect.sh            # starts cloudflared access tcp in the background, writes the
                                                   # kubeconfig (exec credential = gha-token.sh, audience
                                                   # yyc-cluster), retries `kubectl version` 3 x 20 s
      - name: plan
        run: |
          tools/yyc-apply/target/release/yyc-apply plan --folder yyc/deploy --inventory yyc-deploy/inventory-yyc-ai \
            --sealed yyc/deploy/sealed --out plan.md
          cat plan.md >> "$GITHUB_STEP_SUMMARY"
      - name: apply
        if: github.event_name != 'workflow_dispatch' || inputs.mode == 'apply'
        run: tools/yyc-apply/target/release/yyc-apply apply --plan plan.md --run "${{ github.run_id }}"
      - name: tell Discord
        if: always() && github.ref == 'refs/heads/main'
        # as cloudflare.yml: a failed run, and the first good run after one, to YYC_DISCORD_WEBHOOK
```

Notes:
- `gha-token.sh` is the kubeconfig's exec plugin: it asks `ACTIONS_ID_TOKEN_REQUEST_URL&audience=yyc-cluster` with
  `ACTIONS_ID_TOKEN_REQUEST_TOKEN` and prints an `ExecCredential` with the token's own `exp`; kubectl calls it again
  when it expires, so a long rollout wait never runs on a stale token. The token is never echoed (the plugin writes
  only to kubectl's stdin pipe) and GitHub masks it in logs.
- The schedule (every 3 h at minute 7, off `cloudflare.yml`'s minute marks) re-applies `main` HEAD: an object deleted or a managed
  field edited by hand is restored, and an object still waiting on a prune (a run that failed after apply) is pruned.
- `concurrency` with `cancel-in-progress: false` queues runs; GitHub keeps one pending run and cancels older pending
  ones (grey, not red), which is right: the newest `main` contains every older commit.
- `yyc-ai-zdr.yml` stays as it is (its daily run and mail, cutover 10.1.1); the `check` job also runs the guard, so a
  non-ZDR model never reaches the cluster.
- `hermit-ami.yml`, `cloudflare.yml`, `mcp-image.yml` are unchanged and run beside it on the same push.

---

## 6. The applier `yyc-apply`

Rust (claude-env "prefer Rust"), `DE0CH/YYC/tools/yyc-apply/`, one binary used by the workflow and from a session.
It drives `kubectl` (server-side apply and dry runs) rather than reimplementing them, and owns the parts kubectl
lacks: an inventory, the deletion gate, ordering and waiting. It knows nothing about YYC AI beyond the flags it is
given, so the news-stream and ai-watch folders can move to it later (11).

### 6.1 Inventory

ConfigMap `yyc-deploy/inventory-yyc-ai`: `data.entries` = the applied object ids, one per line in Flux's own format
(`<namespace>_<name>_<group>_<Kind>`, plus `apiVersion`), and annotations `yyc-deploy/commit` (the full sha
applied), `yyc-deploy/run` (run id), `yyc-deploy/applied-at`, `yyc-deploy/phase` (`applying` or `done`). Before an
apply it writes old ∪ new with phase `applying`; after a full success it writes new with phase `done`. A run that
dies anywhere leaves a superset, so nothing it created is ever forgotten and the next run prunes what is due.

### 6.2 Stale runs

`apply` compares its checkout with `yyc-deploy/commit`: if the inventory's commit is a descendant of this run's sha
(an old run re-run after a newer one applied), it exits green with "main already at <sha>: nothing to do". Rolling
back is a revert commit, never a re-run of an old run. An unrelated history (impossible without force-pushes, which
are banned) fails red.

### 6.3 Plan and the deletion gate

`plan` renders (`kustomize build`, the same 5.8.1 the check uses; the hand-over proves Flux's build and this one
agree), then for every object runs a server-side dry-run apply with field manager `yyc-ai-deploy` and reports:
creates; updates with the changed fields (managedFields and `status` ignored); immutable-field changes (the apply
would fail); the **prune set** = inventory − render.

Each prune-set member must be named by a trailer in a commit in `yyc-deploy/commit..HEAD`:

| Trailer | Effect |
|---|---|
| `Deploy-Prune: <id>[, <id>…]` | delete these objects after the rollout (6.4 step 6) |
| `Deploy-Prune-Namespace: <ns>` | allows every prune-set member in `<ns>` (the teardown of `jarvis`); the namespace itself is a cluster-level object (4.2), removed by its own `clusters/hetzner` commit afterwards |
| `Deploy-Release: <id>` | drop from the inventory, keep the object (an object moving to another owner, e.g. the HelmRelease in 8) |
| `Deploy-Replace: <id>` | delete then create (for an immutable-field change); refused by `check` for Deployments, DaemonSets, StatefulSets, PDBs and Services in `yyc-ai` (rename instead: the new object serves before the old one is pruned) and for PVs/PVCs |

Exempt from trailers: ConfigMaps and Secrets whose name is a kustomize generator name plus its hash suffix, when the
same generator still renders a newer one (a `hermit.json` edit leaves `hermit-config-<old hash>` behind; Flux pruned
those silently and so does this). An unnamed prune-set member fails the run before anything is applied, with the
exact trailer lines to add. A trailer naming something not in the prune set is a warning only.

`yyc-apply plan --trailers` from a session prints those lines; this is claude-env's "before a push that deletes or
replaces infra, run the plan against the live state". From a session it reaches the API through a new
the new cluster's operator path (DESIGN 15.2 N2: an admin kubeconfig from store `yyc`, written 0600 to `$SECRETS`,
never printed).

### 6.4 Apply order (one run)

1. **Cluster-scoped and cross-namespace objects** of the render (PV `hermit-jfs`; the bridge placeholders in `yyc`;
   after yyc-mcp's move, `yyc/yyc-catalogue`).
2. **Sealed Secrets**: `kubectl apply --server-side --as=system:serviceaccount:yyc-deploy:sealed-secrets-applier`
   of `sealed/` (which `check` requires to hold only `kind: SealedSecret`). Then, for each SealedSecret whose
   `metadata.generation` changed, wait (≤ 2 min) until `status.observedGeneration == metadata.generation` and
   condition `Synced=True`. Pods therefore never roll before their new Secret exists, which retires the "bump
   `secrets-rev` twice" failure; `secrets-rev` stays the way to roll a reader (spec-platform 9.2).
3. **Everything that is not a workload**: ConfigMaps (new hashed names appear before any Pod refers to them),
   ServiceAccounts, Roles, RoleBindings, Services, NetworkPolicies, PDBs, LimitRanges, PVCs.
4. **Workloads**: Deployments, DaemonSets, StatefulSets, HPAs, CronJobs, Jobs. One `kubectl apply --server-side
   --field-manager=yyc-ai-deploy` per phase, `--force-conflicts` (this manager is the owner of what git says; an HPA's
   `replicas` is never in the manifests, so the HPA keeps owning it).
5. **Wait for readiness**: `kubectl rollout status --timeout=15m` for every Deployment, DaemonSet and StatefulSet in
   the render (cheap when unchanged). Every Deployment rolls `maxSurge 1 maxUnavailable 0` with readiness on
   `:9090/readyz` and PDBs (spec-platform 9.3, enforced by `check`), so old Pods serve until each new one is ready;
   drains (egress's 625 s grace) run after `rollout status` returns and do not hold the run. One-off Jobs in `jobs/`
   are not waited on (the cutover runner watches its own Jobs).
6. **Prune**, only after step 5 succeeded: workloads first, then the rest, then cluster-scoped; `kubectl delete
   --wait --timeout=10m` each; old NetworkPolicies and ConfigMaps go last so a draining old Pod keeps them until it is
   gone from the rollout.
7. **Inventory** to the new set, phase `done`.

Order inside one commit still does not matter to the services (every component accepts its peers' previous
protocol, spec-platform 3.4); the order above is only about Kubernetes references (ConfigMaps, Secrets and policies
existing before the Pods that need them, old ones outliving the Pods that used them).

A step that fails stops the run red: nothing later runs, nothing is rolled back automatically (git is the source:
fix forward or revert), and old Pods keep serving because nothing removed them.

### 6.5 Drift and restarts

Server-side apply restores only fields this manager owns. A `kubectl rollout restart` writes
`kubectl.kubernetes.io/restartedAt` under manager `kubectl-rollout`, which is not undone (under Flux it was). So an
operator restart now sticks; a roll that must survive a rebuild still goes through git (`secrets-rev`, an image pin, a
hashed ConfigMap).

### 6.6 What `check` adds to spec-platform 13.3

9. Every rendered (namespace, kind, name) is inside 4.2's table.
10. `sealed/` renders only `kind: SealedSecret`, each with a matching `infra/secrets.txt` line.
11. Trailers in the pushed commits are well-formed and `Deploy-Replace` is not used on the refused kinds.
12. The render contains no `Namespace` object and no RBAC outside `yyc-ai`, `yyc-ai-fabric`.
13. One cluster: the render contains no object in `jarvis`, `kube-system` or `flux-system`, and nothing under
    `yyc-ai/legacy/` is referenced from `yyc/deploy/` (DESIGN 15.4).

Tests (`tools/yyc-apply/tests`, against a kind cluster in the `check` job when the applier changes): prune gate (an
unnamed removal fails before any write), inventory superset after a killed run, stale-run skip, SealedSecret wait,
rollout wait with a never-ready Pod (red, old Pod still serving), field-ownership transfer from a fake
`kustomize-controller` manager.

---

## 7. Credentials: where each lives and how it reaches GitHub without being printed

| Credential | Lives in | Reaches | How |
|---|---|---|---|
| GitHub OIDC token | minted per kubectl call by GitHub | the API server | `id-token: write` on the `deploy` job only; nothing stored |
| `YYC_DEPLOY_ACCESS_CLIENT_ID`, `YYC_DEPLOY_ACCESS_CLIENT_SECRET` (Access service token) | store `yyc` (source of truth) | GitHub environment `yyc-ai` secrets | tofu output → file 0600 → `scripts/store-put.py yyc KEY=file:PATH --explain KEY=…` → `gh secret set` from the same file → file deleted |
| `YYC_CF_DEPLOY_TUNNEL_TOKEN` | store `yyc` | SealedSecret `yyc-deploy/tunnel` (`infra/secrets.txt` line, `infra/seal.py`) | never GitHub |
| `YYC_DISCORD_WEBHOOK` | existing repo secret | the Discord step | unchanged |
| Cluster CA | `infra/yyc-talos-ca.pem` (public; the new cluster's) | the kubeconfig | committed; `infra/yyc-cluster-ca.pem` (the old cluster's) stays only for the importer's link B7 and goes with the old cluster |
| `YYC_CI_SECRETS_TOKEN` | store `yyc` | used by Claude to set the secrets above | must carry Environments write and Secrets write on DE0CH/YYC; if it lacks Environments, a fine-grained token is minted with the `github-web` skill and stored the same way |

The commands, run in a session holding store `yyc` (each value travels file → stdin, never argv or stdout):

```bash
D=$(mktemp -d) && chmod 700 "$D"
( cd news-stream/deploy/cloudflare/tofu && session-secret --keys YYC_TOFU_R2_ACCESS_KEY_ID,YYC_TOFU_R2_SECRET_ACCESS_KEY --exec sh -c \
  'AWS_ACCESS_KEY_ID=$YYC_TOFU_R2_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY=$YYC_TOFU_R2_SECRET_ACCESS_KEY tofu init -lockfile=readonly >/dev/null &&
   for o in yyc_deploy_tunnel_token yyc_deploy_access_client_id yyc_deploy_access_client_secret; do
     AWS_ACCESS_KEY_ID=$YYC_TOFU_R2_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY=$YYC_TOFU_R2_SECRET_ACCESS_KEY tofu output -raw $o > '"$D"'/$o; done' )
scripts/store-put.py yyc YYC_CF_DEPLOY_TUNNEL_TOKEN=file:$D/yyc_deploy_tunnel_token \
  YYC_DEPLOY_ACCESS_CLIENT_ID=file:$D/yyc_deploy_access_client_id YYC_DEPLOY_ACCESS_CLIENT_SECRET=file:$D/yyc_deploy_access_client_secret \
  --explain YYC_CF_DEPLOY_TUNNEL_TOKEN="connector token of tunnel yyc-deploy (the k8s API for YYC's deploy workflows); sealed into yyc-deploy/tunnel" \
  --explain YYC_DEPLOY_ACCESS_CLIENT_ID="Access service token 'yyc-deploy (GitHub Actions)': opens k8s-deploy.yyc-internal.com; GitHub env yyc-ai secret" \
  --explain YYC_DEPLOY_ACCESS_CLIENT_SECRET="secret of that service token; GitHub env yyc-ai secret"
session-secret --keys YYC_CI_SECRETS_TOKEN --exec sh -c \
  'export GH_TOKEN=$YYC_CI_SECRETS_TOKEN
   gh api -X PUT repos/DE0CH/YYC/environments/yyc-ai --input - <<< "{\"deployment_branch_policy\":{\"protected_branches\":false,\"custom_branch_policies\":true}}" >/dev/null
   gh api -X POST repos/DE0CH/YYC/environments/yyc-ai/deployment-branch-policies -f name=main >/dev/null
   gh secret set YYC_DEPLOY_ACCESS_CLIENT_ID --env yyc-ai -R DE0CH/YYC < '"$D"'/yyc_deploy_access_client_id
   gh secret set YYC_DEPLOY_ACCESS_CLIENT_SECRET --env yyc-ai -R DE0CH/YYC < '"$D"'/yyc_deploy_access_client_secret'
rm -rf "$D"
# then seal from the store (the env of a session that has the new key, i.e. the next session or session-secret):
session-secret --keys YYC_CF_DEPLOY_TUNNEL_TOKEN --exec infra/seal.py yyc-deploy/tunnel
```

Rotation of the service token: taint the tofu resource, re-run the block above; the old token stops working when
tofu replaces it, so the next run uses the new one (a run in flight at that moment fails red and is re-run).

---

## 8. No hand-over from Flux (decision 13)

The hand-over this section specified (H0-H4: seeding the inventory from Flux's, detaching Flux with
`deletionPolicy: Orphan`, adopting field ownership, one k3s restart of `yyc-master1` for the authenticator) is not
done. Decision 13 puts the new stack on the new cluster, which `yyc-ai.yml` creates from empty, and leaves the old
stack on the old cluster, where Flux keeps applying it until the Talos session removes that cluster (T6). The only
change on the old cluster's Flux is the folder move of DESIGN 15.4 (WP-39): Kustomizations `yyc-ai` and
`yyc-ai-secrets` read `yyc-ai/legacy/` and `yyc-ai/legacy/sealed/` instead of `yyc/deploy/`, proven by an
identical render before `flux-sync.yaml` changes, with `prune: false` held over the move. The bring-up of the deploy
path on the new cluster is 12.5.

---

## 9. Failure modes

| Failure | Effect | Handling |
|---|---|---|
| A commit drops an object without a trailer | `deploy` red before any write; the cluster is unchanged | add the trailer in a follow-up commit (the run's summary prints it) or restore the object |
| A rollout never becomes ready | `deploy` red at 6.4 step 5; old Pods serve (`maxUnavailable 0`); no prune ran | revert the commit (a revert that removes objects carries its trailers: `yyc-apply plan --trailers`) |
| A resealed Secret the controller cannot decrypt | red at 6.4 step 2; no workload applied | reseal (`infra/seal.py`), push |
| Tunnel or Access down, a connector Pod gone | `connect` retries 3 × 20 s, then red; nothing applied | the next push or the scheduled run retries; Discord has the red run; `yyc-deploy` has 2 connectors |
| GitHub's OIDC JWKS unreachable from the control nodes | 401 for the run; nothing applied | resolves with WARP/GitHub; `kubectl get --raw /readyz?verbose` names the authenticator |
| A control node down or upgrading | the other two serve the API; the run goes on (the connector reaches any endpoint) | — |
| All three control nodes down | no deploy; Pods serve | the Talos session's runbook (etcd snapshots in Object Storage) |
| A run dies mid-apply (runner lost) | inventory is the superset (6.1); objects already applied stay | re-run, or the next scheduled run finishes it |
| An old run re-run after a newer one applied | green no-op (6.2) | — |
| Two sessions push at once | runs queue (concurrency); the newest `main` is applied | — |
| Actions minutes run short | GitHub stops runs once the account's included minutes are used (unless a spending limit allows more); nothing breaks, deploys wait | measured before P1 (1, last bullet); if the measured total does not fit, the drift schedule goes to daily, and if pushes alone do not fit, that is a spend question for Deyao |

---

## 10. Changes to the other documents

(These are decision 1's edits, already made. Decision 13 changes several of them again; 12.6 lists those.)

### 10.1 spec-platform.md

| Where | Change |
|---|---|
| Contents item 13 and the 13 heading | "Flux, the deploy flow, the YYC checks and the AMI workflow" → "The deploy workflow, the YYC checks and the AMI workflow" |
| 5 (line 675: "fails the YYC commit before Flux sees it") | "fails `yyc-ai.yml`'s `check` job, so `deploy` never applies the pin" |
| 9.1 layout | remove `namespace.yaml` (namespaces are cluster-level, `clusters/hetzner/yyc-ai-access.yaml`, with their PSS labels), `juicefs-csi.yaml` (→ `clusters/hetzner/juicefs-csi.yaml`) and `sealed-secrets-rbac.yaml`; `sealed/` "applied by the deploy workflow's fenced step (impersonating `flux-system/sealed-secrets-applier`)"; "so Flux applies no change to the running old stack" → "so the deploy's plan shows no change to the running old stack"; "no reconcile order of Kustomizations `yyc-ai` and `yyc`" → "no order of the `yyc-ai` deploy and Kustomization `yyc-apps`" |
| 9.2 | `resources` loses `namespace.yaml` and `juicefs-csi.yaml` |
| 9.4 last paragraph | "Flux must not reset it on every reconcile" → "the deploy's server-side apply must not own it" |
| 9.11 RBAC | add rows of 4.2 (`gha:yyc-ai`); the fence row: bindings now in `clusters/hetzner/yyc-ai-access.yaml`, used by the workflow by impersonation |
| 12.2 | new line `yyc-deploy/tunnel  Opaque  news-stream/deploy/apps/sealed/yyc-deploy-tunnel.yaml  token=YYC_CF_DEPLOY_TUNNEL_TOKEN` |
| 12.3 | new store keys `YYC_CF_DEPLOY_TUNNEL_TOKEN`, `YYC_DEPLOY_ACCESS_CLIENT_ID`, `YYC_DEPLOY_ACCESS_CLIENT_SECRET` (section 7's explanations) |
| 12.4 | DE0CH/YYC environment `yyc-ai` (branch `main` only): `YYC_DEPLOY_ACCESS_CLIENT_ID`, `YYC_DEPLOY_ACCESS_CLIENT_SECRET`; no cluster credential (OIDC) |
| 13.1 | replaced: "Kustomizations `yyc-ai` and `yyc-ai-secrets` no longer exist (deploy-workflows 8). `yyc/deploy` is applied by `yyc-ai.yml` (deploy-workflows 5-6). `yyc-db` (Flux, prune false) still carries 11.1." |
| 13.2 diagram | "yyc-ai-check.yml (13.3) on the push; red = fix forward, Flux has applied nothing invalid" → "`yyc-ai.yml` job `check`; red = nothing applied"; "Flux reconcile (≤ 10 min; or …)" → "job `deploy`: plan, gate, apply, `rollout status` of every workload (minutes after the push)"; the verify box keeps the ops page verdict and `live_check.py`, drops the separate `rollout status` (the run did it); red → "revert the YYC commit (with its `Deploy-Prune` trailers if the revert removes objects)"; "Flux applies the whole set at once" → "one run applies the whole set in the order of deploy-workflows 6.4" |
| 13.3 | it becomes job `check` of `yyc-ai.yml` (not its own file; WP-31's `yyc-ai-render.yml` folds in too); "No cluster credentials" stays true for this job; add items 9-12 of 6.6 |
| 13.4 | replaced by deploy-workflows 6.5: a `rollout restart` now stays; rolls that must persist still go through git |
| 18.1 step 6 | "Flux applies; the old stack is unchanged" → "the deploy applies (its plan: additions only, no trailer); the old stack is unchanged"; P0 list gains "the deploy path and the Flux hand-over (deploy-workflows 8)" before item 1 |
| 18.4 | "delete `legacy/` (Flux prunes namespace `jarvis`: …)" → "delete `legacy/`; the commit carries `Deploy-Prune-Namespace: jarvis` and `Deploy-Prune:` for PV `yyc-ai-jfs`, RuntimeClass `gvisor`, `kube-system/sbx-overlay-route`; the deploy deletes them after its rollout check; then a `clusters/hetzner` commit removes Namespace `jarvis` and its RoleBindings (Flux `yyc-apps` deletes the now-empty namespace) and the deployer's `jarvis` RoleBinding, the `kube-system` Role and the `gvisor`/`yyc-ai-jfs` names in 4.2" |
| 19 rows | "before Flux sees the pin" → "before `deploy` runs"; row "Flux stuck" → "`deploy` red (connect, gate, rollout)": nothing new applies, running state fine; the run summary names the step; re-run after the fix. Row "A resealed Secret not yet decrypted": now prevented (6.4 step 2) |
| 21 Q1 | removed (decided: workflows, Deyao 2026-11-02) |
| Appendix A7.F0 | "kept: Flux Kustomizations…" → "changed: `yyc-ai.yml` applies `yyc/deploy` (deploy-workflows); Kustomizations `yyc-ai`, `yyc-ai-secrets` deleted with `deletionPolicy: Orphan`"; A18.F16 (JuiceFS CSI HelmRelease) → "moved to `clusters/hetzner/juicefs-csi.yaml`, unchanged" |

### 10.2 spec-cutover.md

| Where | Change |
|---|---|
| 1, GitOps row (line 52) | "`yyc/deploy` applied by workflow `yyc-ai.yml` (server-side apply, prune by trailer, waits for rollouts); tofu by `cloudflare.yml`; Flux keeps `yyc-apps`, `yyc-db` (prune false), `yyc-aiwatch`" |
| D16 (line 138) and 11.1 `flip` | "Kustomization `yyc`" → "Kustomization `yyc-apps`" (no Kustomization is called `yyc`) |
| P0 checklist | add: the deploy path works (H0-H4 done; a no-op commit gives a green run; `kubectl auth can-i --list --as=gha:yyc-ai` matches deploy-workflows 4.2) |
| 10.1 commit order | a commit 0 before commit 2: nothing to add for namespaces (created in H0.b); commit 3's "the fenced applier's RoleBindings in `yyc-ai` and `yyc-ai-fabric`" moves to H0.b (cluster level) |
| 10.1 commit 2 text | "Flux applies the new objects; the old ones are unchanged (the P0 render check, and `flux diff kustomization yyc-ai` showing only additions)" → "the deploy applies the new objects; the old ones are unchanged (the P0 render check, and the run's plan showing only creates, no updates in `jarvis`, empty prune set)" |
| 10.1 rollback | "revert commits 5, 4, 3, 2 (Flux prunes the new namespace…)" → "revert commits 5, 4, 3, 2; the revert of 2 carries `Deploy-Prune:` for every new object (from `yyc-apply plan --trailers`); the namespaces stay (empty) until a `clusters/hetzner` commit removes them if the project is abandoned" |
| 10.1 checklist | "Rolling restart of each new Deployment (a no-op annotation bump in git)" stays as written (a git bump); `kubectl rollout restart` is now also allowed and not undone |
| 11.1 runner | "`kubectl` through `cluster.sh ssh`" stays for exec and reads; every "pushes, reconciles" and `flux reconcile kustomization yyc-ai --with-source` → "pushes, then waits until `yyc-deploy/inventory-yyc-ai`'s `commit` is this commit or a descendant with phase `done` and that run is green (`gh run list -w yyc-ai.yml --json`)"; the runner gains `gh` read access only (no new credential: the push still goes through the deploy key) |
| 11.1 `flip` | "waits for the `cloudflare.yml` run to succeed and for Kustomizations `yyc-ai` and `yyc` to be Ready at the new revision" → "waits for the `cloudflare.yml` run and the `yyc-ai.yml` run of the commit to succeed, and Kustomization `yyc-apps` Ready at the new revision (monitor.yaml)" |
| 11.1 `cut-old-connectors`, `retire-ui`, `import-done` | drop `flux reconcile`; the deploy's own `rollout status` replaces the separate readiness waits where they wait for new Pods; the waits for old Pods to disappear stay |
| 11.2 preflight | "No Flux Kustomization not Ready" → "the last `yyc-ai.yml` run green with an empty plan; no Flux Kustomization not Ready" |
| P5.4 (line 1085) | "Flux prunes namespace `jarvis` and everything in it (…)" → "the deploy deletes everything in namespace `jarvis` and the listed cluster-scoped objects (`Deploy-Prune-Namespace: jarvis` and `Deploy-Prune:` trailers, planned from the session first); then the `clusters/hetzner` commit of spec-platform 18.4 removes the namespace" |
| 18 deletion table row "Namespace `jarvis` …" | "Flux prune" → "deploy prune (trailers), then the namespace by `yyc-apps`" |
| 16 failure row "The flip's tofu run fails" | "Flux may already have applied the commit's catalogue and monitor parts" → "the `yyc-ai` deploy and `yyc-apps` may already have applied the commit's catalogue and monitor parts" |
| Appendix A7.F0, A19.F24 | "kept unchanged" → "changed: the `yyc-ai.yml` workflow (deploy-workflows)" |
| switch-A (already says "applied by the YYC workflow") | no wording change; its tab mover DaemonSet in `yyc-ai-fabric` is within 4.2's rights; its removal at A5 needs a `Deploy-Prune:` trailer, which the runner's `retire-old` adds |

### 10.3 DESIGN.md

- 11 "GitOps" bullet → "`yyc/deploy` is applied by workflow `yyc-ai.yml` (deploy-workflows.md): GitHub OIDC
  identity `gha:yyc-ai`, tunnel `yyc-deploy` behind Access, server-side apply, prune only by commit trailer, waits for
  every rollout; `yyc-db` (Flux, prune off) owns databases and roles; tofu by `cloudflare.yml`."
- 11 Layout: drop `namespace.yaml`, `juicefs-csi.yaml`.
- 14 Q1 removed (decided).

### 10.4 WORK-PACKAGES.md

- New **WP-37 Deploy workflow and applier (M)**, Wave 2: `tools/yyc-apply`, `.github/workflows/yyc-ai.yml`,
  `.github/actions/k8s-tools`, `cluster.sh api-forward`, `master-authn.sh`, tofu `deploy.tf`,
  `clusters/hetzner/{deploy-access,yyc-ai-access,juicefs-csi}.yaml`. Acceptance: 6.6's tests green; H1's plan empty
  against the live cluster.
- New **WP-39 Flux hand-over (S)**, Wave 3, before WP-40: H0-H4 with the checks of section 8 in the task record.
- WP-31: its workflows become the `check` job of `yyc-ai.yml`. WP-35: the runner waits on runs, not reconciles.
- Wave 3 preamble: "read-write cluster access through Flux commits" → "through commits applied by `yyc-ai.yml`".

---

## 11. What stays on Flux, and why

On the **old cluster** everything stays on Flux until the Talos session removes it (T6): `yyc-apps`,
`yyc-apps-secrets`, `yyc-db`, `yyc-aiwatch(-secrets)` and, for the old stack, `yyc-ai` and `yyc-ai-secrets` (from
`yyc-ai/legacy/`). On the **new cluster** there is no Flux (the Talos plan deploys by GitHub workflows): its
cluster-level objects, YYC AI's namespaces and RBAC, the deploy path itself and the databases are applied by that
cluster's own workflow (the Talos session's), and `yyc/deploy` by `yyc-ai.yml`. The reason is the one decision 1
gave: the deploy path must not be deployed by the workflow it carries (a bad push could cut the path its own fix
needs), and the deployer's rights must not be granted by the deployer. `yyc-apply` and the tunnel are built so that
another folder on the new cluster is another environment in the authenticator, another inventory ConfigMap and
another workflow.

---

## 12. Decision 13: the new cluster is the target

### 12.1 Facts it rests on (2026-11-03)

| Fact | Source | Consequence |
|---|---|---|
| The new cluster: Talos, three control nodes (3 × cx23 in hel1 planned), workers rebuilt from old ones, KubeSpan between nodes, nodes in any location, IPv6-only with WARP as a WireGuard interface per node, deploys by GitHub workflows, its own Sealed Secrets key | the Talos plan (https://1e6d165c-page.deyaochen.com/), written by session s4ba5e76eb6ec7ca4, which is archived (transcript search, 2026-11-03) | the target, its CA and authenticator are new (12.2); no Flux there (11); the builder is a session Deyao starts, and the bootstrap-time inputs reach it before T1 (DESIGN 15.2.1) |
| Both clusters in Hetzner project `yyc` and network `yyc` during the move | the plan, "The move, step by step" | the bridges of DESIGN 15.5 |
| Nothing of the new cluster is built; the plan waits for Deyao's review | that session's transcript, 2026-11-03 | WP-59's live part, WP-39 and WP-40 wait for T1-T2 (DESIGN 15.2) |
| `yyc-apply`, `yyc-ai.yml` (deploy job off), `master-authn.sh`, `cluster.sh api-forward`, tofu `deploy.tf` (off) and `clusters/hetzner/{deploy-access,yyc-ai-access,juicefs-csi}.yaml` (not listed) exist in DE0CH/YYC, built for the old cluster (WP-37) | DE0CH/YYC at 5fa18403 | 12.4 lists what is kept, moved and deleted |

### 12.2 The path and the authenticator on the new cluster

- Tunnel `yyc-deploy`, its Access app and service token, the GitHub environment secrets: as sections 3 and 7. Only
  the connector's place changes: `news-stream/deploy/clusters/talos/deploy-access.yaml`, applied by the new cluster's
  workflow, egress to the API server as a `CiliumNetworkPolicy` with `toEntities: [kube-apiserver]` on 6443 (DESIGN
  15.2 N3: under Cilium an `ipBlock` of a control node's address does not match) instead of `10.0.0.3/32:6443`. The tunnel token is sealed
  for the new cluster (`infra/sealed-secrets-talos.pem`).
- The authenticator of 4.1, same claim rules and the same `gha:yyc-ai` mapping, in the control nodes' Talos machine
  config: the refactor supplies the patch and file of 4.1 before T1 (DESIGN 15.2.1 input I4), the builder applies
  them. Its JWKS fetch uses each control node's WARP link.
- Kubeconfig: `https://127.0.0.1:6443` (SAN required, N1), CA `infra/yyc-talos-ca.pem`, exec credential
  `gha-token.sh` unchanged.
- From a session: the new cluster's operator path (N2) for `yyc-apply plan --trailers` and the runner's reads.

### 12.3 RBAC on the new cluster

4.2's table as rewritten: namespaces `yyc-ai`, `yyc-ai-fabric`, `yyc-deploy`; the deployer gains
`discovery.k8s.io/endpointslices` (the bridge Services) and a Role in `yyc` for the placeholders `yyc-mcp` and `web`;
PV `hermit-jfs` only; no `jarvis`, no `kube-system`, no RuntimeClass; the fence is
`yyc-deploy/sealed-secrets-applier`. `kubectl auth can-i --list --as=gha:yyc-ai` on the new cluster must match it
(N1's check).

### 12.4 What changes in the implemented applier and workflow (WP-37's delta)

| Piece | Change |
|---|---|
| `yyc-apply seed`, `snapshot`, `adopt` (and their tests: field-ownership transfer from a fake `kustomize-controller`) | deleted (no hand-over) |
| `yyc-apply`'s gated-rollout support (`Deploy-Gated-Rollout`, the hook that runs `cutover ui-gate`, `GATE_WAIT_SECS`) | deleted; the A1 gate runs from the runner against the old cluster with Flux suspended (DESIGN 15.3) |
| `yyc-ai.yml` env `NAMESPACES` | `yyc-ai,yyc-ai-fabric` |
| `yyc-ai.yml` env `HANDOVER_EXCLUDE`, `ACCESS_PENDING` and the header's hand-over state comments | deleted |
| `yyc-ai.yml` env `ACCESS` | `--access news-stream/deploy/clusters/talos/yyc-ai-access.yaml --access news-stream/deploy/clusters/talos/sealed-secrets.yaml` |
| `yyc-ai.yml` paths | `infra/yyc-talos-ca.pem` instead of `infra/yyc-cluster-ca.pem`; the `clusters/talos/` files instead of `clusters/hetzner/`; `yyc-ai/legacy/**` for the `check` job only |
| `yyc-ai.yml` job `deploy` | turned on by WP-59 once N1 holds (its `false &&` removed, `DEPLOY_APPLY` yes from the first run: there is nothing to plan-only against) |
| `check` item 13 (one cluster) | new (6.6) |
| `tools/yyc-apply/connect.sh` | unchanged (hostname and service token are the same) |
| `news-stream/deploy/cluster/master-authn.sh`, its `hetzner-k3s.yaml` `additional_pre_k3s_commands` line and `cluster.sh`'s embedding of it, `cluster.sh api-forward` | deleted |
| `clusters/hetzner/{deploy-access,yyc-ai-access,juicefs-csi}.yaml` | moved to `clusters/talos/` and rewritten for 12.2-12.3 (no `jarvis`, `kube-system`, `flux-system`) |
| tofu `deploy.tf` | turned on (applied by `cloudflare.yml`) in WP-59 |

### 12.5 Bring-up order on the new cluster (replaces H0-H4)

Each step is checked before the next; none touches the old cluster or YYC AI's running stack.
1. **After T1-T2**, built with DESIGN 15.2.1's inputs: the builder's cluster is up with N1-N12 (DESIGN 15.2); `talos-ready.sh` green except the
   items this list makes true.
2. Tofu `deploy.tf` on (`cloudflare.yml` green); credentials of section 7 (store, GitHub environment `yyc-ai`; the
   tunnel token sealed for the new cluster).
3. `clusters/talos/{deploy-access,yyc-ai-access,juicefs-csi}.yaml` applied by the new cluster's workflow: namespaces,
   the deployer's RBAC, the fence's bindings, the connector (2 connectors on the tunnel), the JuiceFS CSI driver.
4. The authenticator in the control nodes' machine config (the builder applies 4.1's patch, one node at a time);
   a `workflow_dispatch` of `yyc-ai.yml` in plan mode authenticates as `gha:yyc-ai` and `kubectl auth can-i --list`
   matches 12.3.
5. `yyc-ai.yml`'s deploy job on; a no-op commit (an empty `yyc/deploy/` render plus a comment) gives a green run
   with an empty plan and an inventory ConfigMap at phase `done`.

Then WP-40 applies the new stack through it (cutover 10.1).

### 12.6 Changes to the other documents

- DESIGN.md: section 15 (text of record), 11 rewritten, rows D26-D35, 9.6-9.8 (the gate with Flux suspended).
- spec-platform.md: its 25 (two clusters), 9.1 (layout without `legacy/`), 9.11 and 13.1 (rights and who applies
  what), 10 (bridge rules), 11-12 (databases, certs), 16.2 (sizes).
- spec-cutover.md: its 23 (two clusters), 7.2 (the importer's credentials across clusters), 10.0-10.1 (P0's live
  work, P1's commit order), 11.1-11.2 (the runner on two clusters), 14 and 16.
- spec-hermit.md: 13.1, 14.2, 14.7, 21.1-21.2, 22.3 and its 30 (the gateway on Talos).
- WORK-PACKAGES.md: WP-37 revised, WP-39 replaced (the `legacy/` folder move), new WP-59 (the new cluster's footing)
  and WP-60 (the bridges), WP-40's dependency on the Talos session's T1-T2.
