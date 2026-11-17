# YYC AI

YYC AI (`https://ai.yyc-internal.com`; the old `ai.yyc.deyaochen.com` answers 301 to the same path there) is the staff
AI chat of YYC: **Claude Code runs in a gVisor sandbox per conversation**, and staff use it through the web and iOS
apps of DE0CH/claudecodeui (`paseo/`, built as the `pigeon-app` bundle). Staff sign in through Cloudflare Access; only
the addresses in the access catalogue's `users` get in ("The staff allowlist"). Design and history: `refactor/` (DESIGN.md, the specs,
DECISIONS); the code: DE0CH/aviary (pigeon, magpie, hermit, sandboxd, the supervisor) and DE0CH/jarvis3 (egress and
tools, image `jarvis3-chat`).

What runs where (the Talos cluster `yyc-talos`, `../news-stream/deploy/clusters/talos/README.md`):
- namespace `yyc-ai`: **pigeon** (the stateless gateway the apps talk to: Paseo's protocol on `/paseo-ws`, the REST
  under `/api`, the app bundle, the operator page's front), **magpie** (the store of record for conversations: events,
  transcripts, blobs, schedules), **hermit** (sandboxes, workspaces, people, the fabric of AWS hosts, the ops snapshot
  and `/ops/`), **egress** (the sandboxes' only way out and the model gateway), **tools** (YYC's read-only tools as a
  REST API), **pg-forward** (the sandbox hosts' way to the JuiceFS database), **cloudflared** (tunnel
  `yyc-ai-pigeon`);
- namespace `yyc-ai-fabric`: the DaemonSet **hermit-overlay-route** (route 10.202.0.0/15 via the sandbox gateway, and
  the rule that puts it ahead of KubeSpan's, on every node in network `yyc`);
- the **sandbox gateway**, outside the cluster: the plain server `yyc-sbx-gw1` (below, "Sandboxes");
- sandboxes: one per conversation on AWS m7i.xlarge hosts (launch template `hermit-host`, eu-central-1), started and
  ended by hermit's scaler, with live migration (packing);
- state: Cluster `magpie-pg` (namespace `db`, three instances, magpie only) and bucket `yyc-ai-magpie` (magpie's
  blobs); hermit, egress and tools on the old k3s cluster's Cluster `pg` (databases `hermit`, `egress`, `tools`)
  through `db/pg-shared` (bridge B3); JuiceFS volume `yyc-ai` (metadata `jfs_yyc_ai` on `pg`, data in bucket
  `yyc-ai-files` prefix `yyc-ai/`): `ws/<conversation>` is each sandbox's `/workspace`; models through OpenRouter key
  `yyc-chat`.

## How a request gets here

```
browser / iOS app ──HTTPS──▶ Cloudflare Access (YYC's account, team yyc-hk; app "YYC AI (staff)" on /api, /paseo-ws,
                     │        /ops; e-mailed code, Google or Microsoft; allow = catalogue users)
                     ▼
                   ai.yyc-internal.com (YYC's zone, proxied record to the tunnel)
                     ▼
                   tunnel yyc-ai-pigeon ── cloudflared (yyc-ai) checks the Access token on those paths
                     ▼
                   pigeon ── magpie (conversations), hermit (a sandbox for the conversation), egress (models list)
                     ▼
                   Claude Code in the conversation's sandbox ── the gateway ── egress ── OpenRouter, tools, labels, magpie
```

**Signed out (Deyao, 2026-10-27, "interface first").** Access covers only `/api`, `/paseo-ws` and `/ops` (tofu
`pigeon.tf` `staff_ai_paths`, which pigeon's own test requires exactly), and cloudflared requires its token on exactly
those paths; the rest of the host reaches pigeon without one, and pigeon serves a signed-out visitor only the app
bundle (the interface with the chat disabled and 「登入」). 「登入」 opens `/api/jarvis/signin`, so Access's login
(branded for YYC: the organisation's `login_design`) signs the person in and pigeon sends them back. The iOS app signs
in through `/api/jarvis/app-signin` (scheme `dev.de0ch.yycai`, `hermit.json` `signIn.appScheme`). A `users` entry
`"@yyc.hk"` = Access's `email_domain` rule.

The Access app, its staff policy (from the access catalogue's `users`), the tunnel and its ingress are OpenTofu on
YYC's Cloudflare account: `/news-stream/deploy/cloudflare/tofu/` (`access.tf`, `pigeon.tf`, `tunnels.tf`; applied by
`cloudflare.yml` on push; `/news-stream/deploy/README.md` "Cloudflare (tofu)").

## Layout

| Path | What |
|---|---|
| `deploy/` | the deploy tree, applied to `yyc-talos` by `.github/workflows/yyc-ai.yml` (below) |
| `deploy/sealed/` | the SealedSecrets (sealed with `/infra/sealed-secrets-talos.pem`), applied by the workflow's fenced step |
| `oneshot/` | Jobs run by hand outside the inventory (`run.sh`, on the images `deploy/` pins): `jfs-owner.yaml` |
| `infra/bridges.sh`, `bridges.json` | the cross-cluster bridges (DESIGN 15.5) |
| `infra/talos-ready.sh` | the new cluster's footing checks N1-N12 (DESIGN 15.2) |
| `infra/make-keys.py` | new store keys (pair tokens, URLs, the fabric PKI) |
| `infra/openrouter.py` | the key `yyc-chat`'s guardrail (models from `deploy/egress/egress.json`; `--verify` checks it live) |
| `tests/` | `check_deploy.py` and its seeded violations, `check_pins_green.py`, the ZDR guard, the bridges and seal tests, `check_pg_ingress.py`, `e2e/` (the live end-to-end) |
| `refactor/` | the design and plan of record of the stack (history; not operational) |

## The deploy tree (`deploy/`, spec-platform 9.1)

- `kustomization.yaml`: image pins (one aviary sha12 for pigeon, magpie, hermit and sandboxd; `pigeon-app` at a
  claudecodeui sha12; `jarvis3-chat` at a jarvis3 sha12) and the ConfigMap generators: `hermit-config` (hashed:
  `hermit/hermit.json`, `system-prompt.md`, `sandbox-rules.md`, `title-system.md`), `hermit-skills` (hashed: the
  managed skills), `catalogue` (fixed name: a change reaches hermit and tools in place), `pg-forward-config`,
  `egress-config`, `tools-config`. The patches carry each Deployment's
  `yyc/secrets-rev`: bump it for the readers of a resealed Secret.
- `limits.yaml`, `network.yaml` (namespace-wide policies; per-component ones sit beside each component),
  `cloudflared.yaml`, `pigeon.yaml`, `magpie.yaml`, `hermit/` (`hermit.yaml`, `fabric.yaml`, `juicefs.yaml` with PV
  `hermit-jfs`), `egress/`, `tools/`, `bridges.yaml`, `branding/` (YYC's logo, tab and home-screen
  icons and web manifest: the app is one build for every deployment and holds neutral ones; ConfigMap
  `pigeon-branding`, which pigeon's initContainer `branding` copies into the bundle's `/branding/`; check item B).
- pigeon and magpie take no behaviour configuration: their env is Secrets only (check item 2). hermit's is
  `hermit.json` (sign-in, sandbox sizes and image by digest, fabric, AWS, ops); egress's `egress.json` (models,
  routes, allowlist options, budget, `exit` = YYC's socks pool); tools' `tools.json` (keys by env name, the capture
  route, yyc-mcp as upstream `data` with its tool list).
- Every Deployment that serves rolls with `maxUnavailable: 0` and at least two replicas; the overlay route is a
  DaemonSet on its nodes.

## Deploying (`.github/workflows/yyc-ai.yml`, `tools/yyc-apply/README.md`)

Edit `deploy/`, `git pull --rebase origin main`, push to main. Job `check` gates: the render and kubeconform (also of
both `db` folders), `tests/check_deploy.py` (no behaviour configuration on pigeon and magpie, env only from Secrets in
`infra/secrets.txt`, the zero-downtime shape, one aviary sha12 and every pin pullable, policies matching workloads,
egress.json against hermit.json, placement in network `yyc` and fsn1, pigeon's sizes, the gateway's host.json, nothing
in `jarvis`/`kube-system`/`flux-system`, magpie-pg's WAL sizes) and its seeded-violation tests, `/infra/seal.py
--check --names-only`, the pinned hermit's `catalogue-check`, `tests/check_pg_ingress.py`, `yyc-apply check`
(deployer rights, the sealed fence, trailers), the ZDR guard, `tests/check_pins_green.py` (every pinned image's commit
passed its own CI: aviary `ci`, claudecodeui `pigeon-app`, jarvis3 `chat`; a pin on a red, running or untested commit
is refused), and the applier's own tests when the applier changed. Job `deploy` plans, applies in order, waits for
every rollout and prunes; it also runs every 3 h (reverting drift) and on dispatch (`mode: plan` only plans). Job
`e2e`, after an applying push or dispatch (not the 3-hourly run), tests the live site with `tests/e2e/live_chat.py`:
signs in as bot@deyaochen.com by Access's e-mailed code (read through the bot-mail Worker's Access-gated
`/access-code`, service token `yyc-ai-e2e` = Actions secrets `E2E_CODE_CLIENT_ID`/`_SECRET`), `/api/jarvis/me`, one
chat turn the model must answer; a red e2e goes to YYC's Discord like a red deploy, screenshots in the run's artifact.

**Test first.** Every YYC AI change, in any of its repos, starts with a test that fails without it (a bug: the test
that reproduces it), committed with the change; the suites run on every push and gate what ships (aviary `ci`,
jarvis3 `chat`, claudecodeui `paseo-ui`, which `pigeon-app` and `yyc-ios` run before building, this workflow).

- **Removing an object** needs its trailer in the commit message: `yyc-apply plan … --trailers` against the live
  cluster prints the `Deploy-Prune:` lines (a revert that removes objects carries them too).
- **Bumps:** one aviary sha12 for pigeon, magpie, hermit and sandboxd, and `hermit.json` `fabric.sandboxd` =
  `aviary-<sha12>`; the sandbox image by digest in `hermit.json` `sandbox.image`; `jarvis3-chat` at a jarvis3 commit
  whose `chat` workflow passed (the ZDR guard reads that tag; `check_pins_green.py` refuses any other).
- **The AMI** follows `hermit.json` by itself: `.github/workflows/hermit-ami.yml` bakes a new `hermit-host` AMI when
  `sandbox.image`, `fabric.sandboxd` or `fabric.runsc` changed (about 15 min) and publishes the sandbox image to the bucket
  `fabric.aws.imageBucket` first; hermit starts a sandbox on a new image only once the bucket holds it.
- **After a deploy** (spec-platform 15.6): `kubectl -n yyc-ai rollout status
  deploy/{pigeon,magpie,hermit,egress,tools,cloudflared}` and the ops snapshot's `health.state` `ok`.

## The managed CLAUDE.md and the skills

Every sandbox's managed CLAUDE.md = `deploy/hermit/system-prompt.md` + `deploy/hermit/sandbox-rules.md` (what lasts,
the `/workspace` quota of `sandbox.workspace` (1 GiB, 1,000 files), installing, internet, YYC's tools); conversation
titles use `title-system.md` with egress's `titleModel`. Claude Code has no MCP servers: it reaches YYC's services
with `curl` as its managed skills say (decision 15; Deyao, 2026-11-04), `deploy/hermit/skills/<skill>/SKILL.md`:
`web` (web search and fetch, social search, AnyAPI: `http://tools/v1/tools/…`), `newswatch` (the monitoring data,
yyc-mcp's tools as `data_*`), `labels` (the label store, `http://labels/api/v2`, `/labels/README.md`), `schedules`
(`http://magpie/v1/sandbox/schedules`: reminders and repeating tasks posted into the conversation), `code` (the
source and deployment of YYC's systems, read-only: below). The three lists
in `kustomization.yaml`, `hermit.yaml`'s items and the folder must match (check item K). A change rolls hermit; a
running conversation picks it up at its next turn (the descriptor's etag: the supervisor rewrites the skills and
restarts Claude Code at an idle point).

## Code access: YYC AI reads its own source and deployment (Deyao, 2026-11-12)

Every conversation can clone, read-only, the four repositories YYC's systems are made of (managed skill `code`):
this repository through egress's sandbox route `code` (`git clone --depth 1 http://code/DE0CH/yyc-code /tmp/code/yyc-code`),
and DE0CH/aviary, DE0CH/jarvis3 and DE0CH/claudecodeui straight from github.com. All four are public: whatever is
committed here is readable by anyone.

- **The route:** `egress.json` route `code` → `https://github.com` (through the socks pool), `allowPaths`
  `/DE0CH/yyc-code/`, `identity: false` (no `Aviary-*` header, so no staff e-mail reaches GitHub; egress's log row still
  names the person), `inject` `Authorization: Basic ${CODE_GITHUB_BASIC}`. Check item 8 keeps those three.
- **The credential:** the fine-grained GitHub token `yyc-ai-code-read` on DE0CH (Contents: read on the private
  DE0CH/YYC only, no expiry; here it reads what anyone can); store `yyc` key `YYC_CODE_GITHUB_BASIC` = base64 of `x-access-token:<token>`, in Secret
  `yyc-ai/egress`. Read-only is the token's: GitHub answers a push 403. Re-mint (claude-env `github-web` skill):
  `github_web.js pat-create-repo yyc-ai-code-read YYC OUT Contents=read`, base64 as above, `store-put.py yyc
  YYC_CODE_GITHUB_BASIC=file:…`, `infra/seal.py yyc-ai/egress`, bump egress's `yyc/secrets-rev`.
- **Another private repository:** its own `/DE0CH/<repo>/` in `allowPaths`, the token re-minted over both (a
  fine-grained token names its repositories), a line in the skill's table.
- **No plaintext secrets may be committed** (they never were: gitleaks over the whole history, 2026-11-12, found only
  sealed files, scraped pages and test values). Secrets stay SealedSecrets.

## What YYC AI may do (Deyao, 2026-10-21; internal writes 2026-11-03)

YYC AI may read anything it is given and change YYC's own internal data through tools built for it (the label store,
schedules), but it never acts outside YYC: nothing that sends, posts, pays or reaches a person. So:

- **No confirmations of any kind.** Nothing asks the staff member to approve anything; Claude Code in the sandbox runs
  with its permissions bypassed (the sandbox is the boundary).
- **Read-only tools only** in tools: `web_search`, `web_fetch`, `social_search`, AnyAPI; WiseSearch is yyc-mcp's
  `wisers_*` tools, seen as `data_wisers_*` (prefix `data`: the agent's tool names carry no company name, 2026-11-02).
  yyc-mcp's tools pass only if listed in `deploy/tools/tools.json` `upstreams[0].tools` (default deny). Never list
  anything that sends mail, posts or changes data outside YYC.
- **Sandbox internet = every site except a deny list** (Deyao, 2026-11-10: fewer false refusals; jarvis3
  docs/CHAT.md "The sandbox internet rules", `generatedAllowlist`): refused are pastebins, file sharing and object
  storage, webhooks and request catchers, tunnels, anyone-runs-a-server platforms, form builders, webmail, open
  proxies/VPNs/shorteners and the other risky UT1 categories; every destination also against Cloudflare's
  malware-blocking DNS; an 8 MiB upload cap per connection (`maxUploadBytes`). A refused host cannot be enabled from
  the chat; the sandbox reads one sentence: "YYC AI egress has denied this network request because the domain name is
  on the blocklist." For YYC only: `denyHosts` in `egress.json` (wins over everything) or `defaultAllowedHosts` (an exception
  to the deny list). Denials are in egress's log (`decision` `deny:denylist`, `deny:config`, `deny:threat`,
  `deny:nxdomain`, `deny:dns`, `deny:address`, `deny:port`, `deny:upload`).

## Sandboxes (hermit's fabric, hermit spec 13-14)

- **Hosts:** AWS m7i.xlarge from launch template `hermit-host` (YYC's account, eu-central-1, tags `app=hermit-host`,
  `hermit=yyc-ai`), started and terminated only by hermit's scaler (`fabric.aws.maxHosts` 5; the account's EC2 vCPU
  quota there is 32). A host starts when a sandbox is asked for and no host has a free slot; it ends once empty and
  older than `minHostLifetimeSeconds` (15 min). Packing live-migrates sandboxes onto the fewest hosts, also while
  Claude Code runs.
- **Capacity (Deyao, 2026-11-13):** a sandbox is cpu 1 / 7Gi with a 100 GiB scratch filesystem (`sandbox.tmpGiB`: the
  root's writes, `/tmp`, Docker; gone with the sandbox, unlike `/workspace`); a host holds 2 (its 15.33 GiB less the
  1 GiB reserve, over 7Gi) on a 220 GiB disk (`fabric.aws.diskGiB`: 2 × 100 + 16 for the system and the image);
  5 hosts hold `maxActive` 10 (check item H keeps the three in step); released after `idleMinutes` 30 without use, at
  most `maxLifetimeHours` 8. After a change of `tmpGiB` or `diskGiB`, hosts launched before it keep their old disk:
  `hermit-admin.sh fresh-host 60`, then `retire` each of them.
- **Network:** sandboxes 10.203.0.0/16, hosts 10.202.0.0/24, the gateway `hermit-gw` at 10.202.0.1 (WireGuard on udp
  51821 over the gateway server's IPv6, `hermit.json` `fabric.gateway.endpoint`), egress VIP 10.202.1.1:3128; Hetzner
  network route 10.202.0.0/15 → the gateway server (10.0.0.8).
- **The sandbox gateway only passes packets** (Deyao, 2026-11-16: a Hetzner route has one next hop, so the gateway
  is single by nature and is kept as simple as possible; every other job is off it). It is the plain server
  `yyc-sbx-gw1` (cx23, fsn1, Ubuntu; no cluster node, no container runtime): WireGuard, sandboxd's fixed rules and
  sandboxd in gateway mode as a systemd unit (aviary `crates/sandboxd/docs/HOST.md` "Gateway on a plain server").
  What it forwards to, all at the one balancer `yyc-ai-egress` (10.0.0.50, Service `egress-lb`): the sandboxes'
  proxy connections (VIP:3128 → 10.0.0.50:3128 → the egress pods) and the hosts' JuiceFS metadata (10.0.0.50:5432 →
  `pg-forward` → `db/pg-shared`). The hosts fetch sandbox images from the S3 bucket, not through it. Its firewall
  `yyc-sandbox-gateway` admits udp 51821 and ICMP from outside; SSH and the API (:7443, hermit's client certificate)
  only over network `yyc`.
- **Changing the gateway** is `infra/gateway.sh`, run from a session, never a workflow: `setup` installs the aviary
  release pinned in `deploy/hermit/gateway.release` (its own pin, not the cluster's aviary pin) and
  `deploy/hermit/gateway-host.json` without losing a packet; `status`; `ssh`. Its WireGuard key and TLS files come
  from the store (`setup --keys`). A reboot or replacement of the server takes every sandbox's network away for its
  duration: run it as the node work of `hermit-admin.sh gateway-maintenance` (below). Replacing the server:
  `gateway.sh move OLD` from the new one's side (the old server's primary IPv6 and the route move; 45 s on
  2026-11-17), with the firewall applied to the new server by id beforehand: applied by label it did not attach when
  the server got its public interface.
- **Operations:** aviary `crates/hermit/scripts/hermit-admin.sh` with `HERMIT_ADMIN_TOKEN` (store
  `YYC_TOKEN_ADMIN_HERMIT`) and `HERMIT_ADMIN` (the operator's email) through `kubectl -n yyc-ai port-forward
  svc/hermit 18080:80`: `hosts`, `cap <host> N|null`, `retire`/`reactivate <host>`, `disable <reason>`/`enable` (new
  sandboxes), `fresh-host`, `sandboxes`, `log`, `release`, `exec`, `recycle <digest>`, `purge <email>` (a leaver),
  `events`; every call is audited in `admin_events`.
- **Gateway maintenance** (hermit 14.7): work that stops the gateway server (a reboot, a kernel upgrade) only through
  `hermit-admin.sh gateway-maintenance --node-work "<command>"` (new sandboxes off, wait for no busy sandbox, the
  command, wait for the gateway, sandboxes on; it leaves new sandboxes off when the gateway does not come back:
  `hermit-admin.sh enable`). Keep the port-forward to hermit alive across it (a hermit roll ends it).
- **JuiceFS:** hermit mounts the volume at `/jfs` (PV `hermit-jfs`, its own CSI mount pods); everything under `ws/` is
  uid/gid 1000 (`oneshot/jfs-owner.yaml` sets it once).

## Zero data retention (Deyao, 2026-10-20)

Staff only ever reach models through YYC's own infra, which keeps every request on Zero Data Retention endpoints:

1. **A fixed shortlist:** `deploy/egress/egress.json` `models` (plus `smallModel`, `titleModel`). Egress's route
   `model` refuses any other model (400, `deny:model`). Every model on it must have ZDR endpoints
   (`GET https://openrouter.ai/api/v1/endpoints/zdr`).
2. **Every request forces ZDR:** jarvis3's egress merges `provider: {zdr: true, data_collection: "deny"}` into the
   body of every request on a route with `"model": true` (`model`), over whatever the caller sent, and refuses a body
   that is not a JSON object (`ModelBodyRewriter`, `chat/src/egress/modelbody.ts`, as the body streams through `proxy.ts`). It is code, not a setting. OpenRouter treats
   per-request `zdr` as OR with the key's and account's settings.
3. **The key's guardrail** `yyc-chat` (`infra/openrouter.py`) is a third layer. Deyao's personal account settings are
   no guard.

`tests/test_zdr.py` (CI `.github/workflows/yyc-ai-zdr.yml` on every change and daily; also a step of `yyc-ai.yml`)
fails when a shortlisted model has no ZDR endpoint, when a route to openrouter.ai is not a forced model route, or when
the pinned jarvis3 commit lacks the forcing or its test.

## Models and budget (Deyao, 2026-10-21: demo stage)

- **Messages sent during a reply, live text, activity, slash commands** (Deyao, 2026-11-12; aviary
  `ca3f2d51c6a7`): the queue above the message box is magpie's (`pendingMessages`): a text message joins the reply
  after Claude Code's next tool result, one with attachments is held until the reply ends and can be taken back. The
  reply's text reaches the app as it is written (the supervisor's live frames). A chip in the header opens the
  activity panel: what the conversation runs beside its reply (a background command, a subagent), with Stop. The
  message box completes Claude Code's slash commands after `/`. The supervisor's part (live text, the command list,
  Stop) needs the sandbox image of that release: a conversation gets it with its next sandbox.
- **Models:** `models` in `egress.json` is what staff pick from (DeepSeek V4.1 Flash, reasoning high; each must take
  images, `tests/test_zdr.py`: the text-only GLM 5.3 failed every call once a conversation held an image, 2026-11-13);
  `defaultModel` is Claude Code's model when they pick none; `smallModel` is Claude Code's small/fast model;
  `titleModel` (`titleReasoning` none) names conversations; `contextTokens` reaches Claude Code as its context size.
  **Model and thinking menu** (Deyao, 2026-11-12): the composer's model button opens the models and, under them, a
  thinking row; a conversation keeps its own pick, a new one starts on `defaultModel` at that model's `reasoning`.
  `models[].thinking` lists the levels a model's OpenRouter endpoints take, lowest first (DeepSeek V4.1 Flash: none,
  low, high, max); the app words them 無, 輕量, 標準, 深入 and
  greys out a level the picked model lacks. hermit says `conversations.peoplePickModel`; magpie keeps the pick and
  sends it with each turn; the supervisor switches Claude Code; egress applies the level the request carries.
  A conversation is also compacted by its images (`hermit.json` `sandbox.compactAtImages`, 30): every model call sends
  all of them again, DeepInfra refuses a request with more than about 31 and the hosts that take more spend 0.1-0.2 s
  on each at every call; at the end of a turn with 30 or more, the supervisor has Claude Code compact (`/compact`, a
  turn of origin agent), which drops them for a summary (the files stay in the workspace).
  A new model: list it, run `infra/openrouter.py` (the key's guardrail lists the allowed models) BEFORE the deploy,
  or its calls get OpenRouter's "Model blocked by guardrail".
- **Shared budget** (`budget` in `egress.json`: `limitUsd` 100, `since`, `ownerName`): everyone together, Deyao
  included; model cost from egress's log since `budget.since`; no automatic reset. Used up: egress refuses every
  model call (402 `budget_exceeded`; the call in flight completes); the ops page shows spend against the limit and
  warns at `ops.budgetWatch` (80%). **Reset or raise only when Deyao says so:** `"since"` = now in UTC
  (`date -u +%Y-%m-%dT%H:%M:%SZ`) resets the spend, a bigger `"limitUsd"` raises it; push.
- **Backstop:** the OpenRouter key `yyc-chat` (only YYC AI) has its own monthly cap (`infra/openrouter.py`
  `LIMIT_USD`), above the app budget; the ops page shows its balance.

## Operator page and View as (Deyao only)

https://ai.yyc-internal.com/ops/ (English, read-only; hermit's page, aviary `crates/hermit/src/ops_page.html`):
the verdict, the Deployments of `hermit.json` `ops.deployments` and their pods, the gateway, hosts, live sandboxes
and owners, the scaler, the budget and OpenRouter balance, errors; history in Hong Kong time. Only `admins` of
the access catalogue get it (hermit checks). The news stream's monitor reads the same snapshot (`/ops/snapshot`, token
`HERMIT_OPS_TOKEN` in `yyc/monitor-keys`) through bridge B4. The Now tab's "View as" card lists every current user with a 「View as」 button:
YYC AI as that person, read-only, with an override to send as them; pigeon honours the view-as token only for an
admin's own Access identity, and every view and send is recorded. `/raw/<conversation>` is the raw transcript for an
admin (audited by magpie).

## The staff allowlist

The access catalogue, `{"users": [...], "admins": [...]}` (`"@yyc.hk"` = everyone there), is the catalogue of EVERY
YYC app (Deyao, 2026-10-28): `users` is the one kind of access (every app and internal tool, including the news
stream's /app and beta, YYC AI, WiseSearch, Mail Merge, AI 觀察, 標籤, 名牌報名, yyc-mcp), and `admins` (Deyao) only
names who gets the operator tools inside the apps (YYC AI's /ops and View as, yyc-mcp's call log). It is people's mail
addresses, so it is never a file in git (Deyao, 2026-11-16; check item A): it lives in the store `yyc` as
`YYC_CATALOGUE_JSON`, and its readers get it from there. hermit and tools read the Secret `yyc-ai/catalogue`
(`sealed/catalogue.yaml`, re-read every 30 s), yyc-mcp checks the Secret `yyc/yyc-catalogue` on every call (key
`config.json`, applied by `yyc-apps.yml`), and tofu builds the Access policy `YYC users` from the Actions secret
`YYC_CATALOGUE_JSON` (`/news-stream/deploy/cloudflare/tofu/access.tf`, variable `catalogue`), which `yyc-ai.yml`'s
catalogue check reads too. Access doesn't send a code to an unlisted address.

To change it: `/infra/catalogue.sh get FILE`, edit, `/infra/catalogue.sh put FILE` (the store key, both sealed files
and the Actions secret in one go), commit and push the two sealed files, and `gh workflow run cloudflare.yml` (or its
next half hour) for the Access policy.

## Taking YYC AI offline, rolling back

- **A bad deploy:** revert the commit and push; `yyc-ai.yml` applies the previous state pod by pod (with the
  revert's `Deploy-Prune` trailers if it removes objects).
- **Offline for staff:** point the Access app `staff_ai` in `/news-stream/deploy/cloudflare/tofu/access.tf` at the
  `admins` policy and push (`users` is every YYC app's list, so emptying it would lock staff out of all of them).
- **No new sandboxes:** `hermit-admin.sh disable "<reason>"` (running sandboxes untouched); all hosts gone:
  `fabric.aws.maxHosts` 0.
- **The cluster lost as a whole** is a stated gap: rebuild from the Talos runbook
  (`../news-stream/deploy/clusters/talos/README.md`), `magpie-pg` from its backups (`pg-pitr`, `pg-dump-magpie`).

## Databases and bridges

- **magpie-pg** (`../news-stream/deploy/clusters/talos/db/magpie-pg.yaml`, applied by `talos-cluster.yml`): planned
  work is a switchover first (`kubectl cnpg promote -n db magpie-pg <standby>`, or `primaryUpdateMethod: switchover`
  on an update), never a primary stop; its WAL sizes are checked. Backups: the nightly base backup to `pg-pitr` and
  `pg-dump-magpie` into `yyc-pg-backups/dumps/`.
- **`pg`** (old cluster, `../news-stream/deploy/db/postgres.yaml`): hermit, egress and tools through `db/pg-shared`;
  its `pg-ingress` only grows (`tests/check_pg_ingress.py`; a commit that shrinks it carries `Pg-Ingress-Shrink:
  <why>`). A restart pauses hermit's starts, egress's budget writes and JuiceFS metadata for its tens of seconds.
- **Bridges** (DESIGN 15.5; `infra/bridges.sh up|check|down|render`, HAProxy on `yyc-bridge1` with the alias IPs
  `yyc-bridge-old` 10.0.255.1 and `yyc-bridge-new` 10.0.255.2): B3 `pg`, B4 the news-stream monitor → hermit, B5/B6
  tools → yyc-mcp and the capture route (placeholders `yyc/yyc-mcp`, `yyc/web` in `bridges.yaml`). Each goes when its
  callee or caller moves to `yyc-talos` (T4, T5); a server joining or leaving a cluster needs `bridges.sh up`.

## Relay

The old stack's relay (staff's own Claude Code on YYC AI's models) is not part of this stack (DESIGN 10, decision 14).

## Secrets

SealedSecrets in `deploy/sealed/` (namespace and name bound: scope strict; sealed to `/infra/sealed-secrets-talos.pem`),
sealed from the store `yyc` with `/infra/seal.py`; the authoritative map of Secret keys to store keys is
`/infra/secrets.txt` (pair tokens: `<CALLER>_TOKEN_<CALLEE>` = `<CALLEE>_ACCEPT_<CALLER>`, store
`YYC_TOKEN_<CALLER>_<CALLEE>`). New keys: `infra/make-keys.py OUT` (random keys and the URLs made from them),
`--derived` (from existing keys), `--fabric AVIARY` (the fabric PKI and the gateway's WireGuard key) write files and
print the `scripts/store-put.py yyc …` command. Change one: put the new value into the store, `infra/seal.py
yyc-ai/<name>`, bump the readers' `yyc/secrets-rev`, commit, push. A NEW Secret must be a SealedSecret too (a line in
`/infra/secrets.txt`, then seal.py); a plain Secret must never be committed. `yyc-ai.yml`'s fenced step applies
`deploy/sealed/` and can create SealedSecrets and nothing else. The key that opens them exists only in the cluster.
