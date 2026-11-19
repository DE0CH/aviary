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
GitHub environment `checks`, open to every branch; nothing in `checks` can deploy.

## main: one deploy run

`deploy.yml` runs on every push to main. It waits until every part's run on that commit is over and green, then:

1. **Images.** Each image is tagged by the last commit that changed its inputs (`deploy/images.json` lists each
   image's paths): an unchanged part keeps its tag, so its pods are not rolled. The part's workflow built that tag
   if the registry lacked it; the deploy run only checks it exists.
2. **Jarvis chat.** `jarvis/deploy` applied to Jarvis 4's cluster (namespace `services`, a credential scoped to
   it), every rollout waited for, then its live end-to-end check.
3. **Jarvis 4's sessions.** The release is committed into DE0CH/jarvis4 `session-image/aviary.json` with that
   repository's deploy key; its push builds the session image, so new aviary sessions run this commit.
4. **YYC AI.** `yyc/deploy` applied to YYC's cluster (yyc-apply, through YYC's Access-gated API), every rollout waited
   for, then its live end-to-end check, the site's install check, and the host AMI baked when the sandbox image,
   sandboxd or runsc changed.
5. **The sandbox gateway.** `yyc/infra/gateway.sh setup` over the jump host (no packet lost); unchanged = nothing done.

Jarvis chat goes first: Deyao's own instance takes a bad build before YYC's staff do. A failed step stops the run
there and goes to Discord; the fix is the next push. A revert is a push too.
