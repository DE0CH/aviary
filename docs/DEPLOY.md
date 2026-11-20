# Deploying: a push to main is the deploy

Deyao, 2026-11-19: "product (or service) is the goal. A dev workflow must not change how the product is delivered…
pushing to main = deploy." This repository holds everything YYC AI and Jarvis chat run, and each deployment's own
configuration, so main is what runs. There is no release step, no pin to move by hand, no second repository to bump.

```
crates/ images/ fabric/ testing/   aviary: pigeon, magpie, hermit, herald, sandboxd, the supervisor, the sandbox image
app/                               the Paseo app: the web bundle pigeon serves (pigeon-app), iOS, Android
egress/                            egress (the sandboxes' model and web gateway) and tools (REST), one image
harness/qwen-code/                 the Qwen Code fork (upstream QwenLM/qwen-code, merged in: -X subtree=harness/qwen-code)
jarvis/                            Jarvis chat (chat.deyaochen.com, Deyao's): deploy/ (its manifests), hermit/ (hermit-jarvis)
yyc/                               YYC AI (ai.yyc-internal.com, YYC's staff): deploy/, infra/, tests/, oneshot/
```

## Branches: every check before the merge

A branch push runs each part's workflow (`ci`, `app`, `egress`, `jarvis`, `qwen-code`, `yyc`) for the parts it
changed, and the gate (`branch.yml`, `merge.yml`) merges the branch into main only once every run on it is green. The
checks that need a secret (Jarvis 4's replica: a Fly token; YYC's catalogue check: the catalogue) get it from the
GitHub environment `checks`, open to every branch; nothing in `checks` can deploy. Jarvis 4's two replicas (`ci.yml`
`replica`, `replica-fabric`) run this commit's release in a Jarvis 4 session before the merge: on a Fly machine, and
as a gVisor sandbox on the runner as the fabric runs it (jarvis4 `rust/e2e-kernel`, `E2E_RUNNER=fabric`), since step
2 below moves the fabric's pin with every deploy.

Only a branch's newest commit is checked (Deyao, 2026-11-20): every workflow that runs on branches has a
`concurrency` group per branch with `cancel-in-progress` off main, so a push that starts a workflow cancels that
workflow's run on the branch's older commit. A workflow the newer push does not start (its paths untouched) keeps its
run: the gate carries that result to the newer commit (jarvis4 `rust/gate` `decide::latest`), so cancelling it would
leave the branch red. main cancels nothing: its runs build what the deploy ships.

## main: one deploy run

`deploy.yml` runs on every push to main. It waits until every part's run on that commit is over and green, then:

1. **Images.** Each image is tagged by the last commit that changed its inputs (`deploy/images.json` lists each
   image's paths): an unchanged part keeps its tag, so its pods are not rolled. The part's workflow builds that tag on
   whichever branch first lacks it (pigeon-app on main only: its image job lists each build in main's
   `testing/paseo-contract/clients.txt`); the deploy run only checks it exists.
2. **Jarvis chat and Jarvis 4's sessions.** Jarvis 4's cluster takes changes only from git (jarvis4 DECISIONS 14), so
   the deploy run pushes the tag `deploy-<sha12>` and makes one commit in DE0CH/jarvis4 with that repository's deploy
   key: the chat tenant (`k8s/cluster/tenants/chat.yaml`: `jarvis/deploy` at that tag, with this commit's image
   tags), the sessions' release (`session-image/aviary.json`; its push builds the session image, so new aviary
   sessions run this commit) and the fabric's tags (`k8s/cluster/fabric`; jarvis4's `fabric-ami.yml` then bakes the
   fabric hosts' AMI with `fabric-aws` when sandboxd, runsc or the disk changed). Flux applies it; the run waits for the
   chat to serve this build, then checks the site through Access.
3. **YYC AI.** `yyc/deploy` applied to YYC's cluster (yyc-apply, through YYC's Access-gated API), every rollout waited
   for, then its live end-to-end check, the site's install check, and the host AMI baked (`fabric-aws`,
   crates/fabric-aws) when the sandbox image, sandboxd or runsc changed.
4. **The sandbox gateway.** `yyc/infra/gateway.sh setup` over the jump host (no packet lost); unchanged = nothing done.

Jarvis chat goes first: Deyao's own instance takes a bad build before YYC's staff do. A failed step stops the run
there and goes to Discord; the fix is the next push. A revert is a push too.
