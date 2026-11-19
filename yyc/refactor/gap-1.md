# Gap 1: front door switches (yyc-edge Worker, offline mode, cut-over flags, cloudflare.py, tofu, the 301)

Researched 2026-11-02. Sources: DE0CH/YYC at 1775efc4, DE0CH/claudecodeui at 3de3f5d8 (the live `ui` image), jarvis3
HEAD, claude-env skills, the live cluster (read-only kubectl), and unauthenticated curl probes of the public hosts.
YYC paths are relative to the YYC repo; `tofu/` means `news-stream/deploy/cloudflare/tofu/`.

## 1. Where things are now (short answer)

- **There is no `yyc-ai/edge/` and no `cloudflare.py` any more.** YYC commit 29660229 (2026-10-23, "Clean cut of the
  Cloudflare move") deleted `yyc-ai/edge/{worker.js,wrangler.jsonc}` (Worker `yyc-ai` on Deyao's account, the one with
  `UNDER_CONSTRUCTION`), `yyc/infra/cloudflare.py` (the one with `UI_CUTOVER` and `--dns`) and
  `news-stream/infra/cloudflare.py`. Everything on Cloudflare is now OpenTofu on YYC's own account, plus one Worker
  deployed by hand.
- **The 301 for ai.yyc.deyaochen.com** is the `MOVED` map in the news stream's router Worker **`yyc-edge`**,
  `news-stream/edge/worker.js:16-17, 56-61`. Its route is in tofu (`tofu/saas.tf:70-78`). The hostname is a Cloudflare
  for SaaS custom hostname (`tofu/saas.tf:30`), and Deyao's DNS record for it is set by hand outside git
  (`news-stream/deploy/README.md:170-185`).
- **No offline page exists.** The only "offline" switch left is an Access-policy swap in tofu
  (`yyc-ai/README.md:118-120`). Section 3.1 explains why it does not log out people who are already signed in.
- **The cut-over flag `UI_CUTOVER` is now the tunnel ingress `service`** in `tofu/tunnels.tf:69,72`
  (`http://ui.jarvis.svc.cluster.local:8080`).

## 2. The front door today, hop by hop

| Hop | What | Where |
|---|---|---|
| DNS (YYC zone) | `ai.yyc-internal.com` is a proxied CNAME to the tunnel `yyc-ai` | `tofu/tunnels.tf:139-147` |
| DNS (Deyao's zone, by hand) | `ai.yyc.deyaochen.com` and `ai-yyc-ui.deyaochen.com` are DNS-only CNAMEs to `customers.yyc-internal.com`. Verified live with DoH: both answer `customers.yyc-internal.com.` | `news-stream/deploy/README.md:170-185`; monitored by `news-stream/monitor.py:1828-1864` (`SAAS_NAMES`, Discord alert) |
| SaaS custom hostnames | `ai.yyc.deyaochen.com` and `ai-yyc-ui.deyaochen.com` both use the custom origin `origin_ai` (= `ai.yyc-internal.com`). `ssl.method = "http"` | `tofu/saas.tf:23-48` |
| Worker route | `ai.yyc.deyaochen.com/*` goes to script `yyc-edge`, along with yyc and yyc-beta | `tofu/saas.tf:70-78` |
| Worker `yyc-edge` | When `MOVED[hostname]` matches, it answers 301 to `https://ai.yyc-internal.com<path><query>` with `X-Edge: MOVED` and sends nothing to the origin | `news-stream/edge/worker.js:10-17, 53-61` |
| Access org | team `yyc-hk`, login page branding | `tofu/access.tf:10-32` |
| Access app "YYC AI (staff)" | hosts `ai.yyc-internal.com` and `ai-yyc-ui.deyaochen.com` × paths `api, paseo-ws, ws, ops, __jarvis`. Policy "YYC users" | `tofu/access.tf:64-65, 164-177` |
| Tunnel ingress | per host: the first rule `^/(api\|paseo-ws\|ws\|ops\|__jarvis)(/.*)?$` goes to `ui:8080` with `origin_request.access.required` (team, aud of staff_ai). The second rule sends the rest of the host to `ui:8080` without a token. Anything else gets `http_status:404` | `tofu/tunnels.tf:59-75` |
| cloudflared | Deployment `cloudflared` in `jarvis`, 2 replicas, maxUnavailable 0, `--grace-period 3m`, PDB minAvailable 1 | `jarvis3/deploy/chat/cloudflared.yaml:1-62`; live: 2/2 `cloudflare/cloudflared:2026.10.0` |
| NetworkPolicy | `ui-ingress` admits port 8080 only from cloudflared, ui and egress pods | `YYC/yyc-ai/deploy/ui.yaml:163-174` |
| Router | checks the Access JWT again plus `users`; serves the signed-out shell | `claudecodeui/jarvis/router.mjs:110-124, 204-345, 580-596, 878-892, 944-974, 1066-1134` |

Live probes (2026-11-02, no credentials):
- `GET https://ai.yyc.deyaochen.com/some/path?x=1` returns `301`, `location: https://ai.yyc-internal.com/some/path?x=1`,
  `x-edge: MOVED`. The same happens for `/paseo-ws` and for `POST /api/jarvis/warm`. A 301 on a POST turns it into a
  GET, and WebSockets do not follow redirects.
- `GET https://ai.yyc-internal.com/` returns 200 with the shell (`<meta name="yyc-signed-out" content="1">`).
  `/yyc-logo.png` returns 200 without sign-in.
- `/api/jarvis/me`, `/__jarvis/x`, `/ops/` and a WS upgrade of `/paseo-ws` with a bogus `cf-access-token` all return
  302 to `https://yyc-hk.cloudflareaccess.com/cdn-cgi/access/login/...`. They also carry
  `www-authenticate: Cloudflare-Access resource_metadata=".../.well-known/cloudflare-access-protected-resource/<path>"`,
  and that `.well-known` URL answers 200 with JSON naming the team.
- `OPTIONS /api/jarvis/me` with `Origin` and `Access-Control-Request-Headers: cf-access-token` returns 403 from Access.
  No CORS or preflight bypass is configured on the app. An OPTIONS with no Origin returns 400.
- `GET /healthz` and `/readyz` on the public host return 200 `{"ok":true,"draining":false,"processes":0}`. They are
  outside the Access paths, and the router answers them before identity (`router.mjs:945-950`), so each replica's
  live per-person process count is readable by anyone.
- `ai-yyc-ui.deyaochen.com` behaves exactly like `ai.yyc-internal.com`: shell 200, `/api` sends the browser to Access.

## 3. Every operator switch that changes what staff reach

| # | Switch | Where it is set | Effect | Status |
|---|---|---|---|---|
| 1 | **Offline**: Access app `staff_ai` policy `users` → `admins` | `tofu/access.tf:175`; runbook `yyc-ai/README.md:118-120` | New sign-ins by non-admins are refused at Access | **Valid but incomplete** (3.1) |
| 2 | **Old chat UI rollback**: pin jarvis3 2156d8a and point tunnel ingress at `web` | `yyc-ai/README.md:120-123`; `tofu/tunnels.tf:69,72` | Would put the pre-2026-10-21 web chat back on the host | **Stale, almost certainly broken** (3.2) |
| 3 | **Tunnel ingress service** (the old `UI_CUTOVER`) | `tofu/tunnels.tf:69,72` | Which Service serves the host and its signed-in paths | **Valid**: this is the cut-over lever for pigeon |
| 4 | **Signed-in path list** | `tofu/access.tf:65` (`staff_ai_paths`), `tofu/tunnels.tf:68` (derived), `router.mjs:112` `SIGNED_IN_PATHS` + `:123` (`/__jarvis` added by hand) | Which paths need a token. Everything else is public shell | **Valid**. Kept in step by hand in 2 repos (`yyc-ai/README.md:43-44`) |
| 5 | **The 301** (`MOVED` map) | `news-stream/edge/worker.js:17` | Old name sends a 301 to the new one | **Valid**. Deployed by hand (3.3) |
| 6 | **Worker route** for the old name | `tofu/saas.tf:71` | Removing it sends `ai.yyc.deyaochen.com` to `origin_ai`. The tunnel has no ingress rule for that hostname (`tunnels.tf:64-73`), so it gets a 404 | Valid |
| 7 | **Custom hostnames** | `tofu/saas.tf:26-32` (+ `test_hostnames` `:25` for throwaway flip tests) | Which deyaochen.com names reach YYC's account | Valid |
| 8 | **Deyao's hand-set CNAMEs** (5 names) | outside git, `news-stream/deploy/README.md:170-185` | Taking one down takes the name off YYC's account | Valid. Monitor alerts on drift (`monitor.py:1830-1864`) |
| 9 | **Test host `ai-yyc-ui.deyaochen.com`** | `tofu/access.tf:64`, `tofu/saas.tf:31`, tunnel rules via `staff_ai_hosts` | A second name on the same `ui`. It was the pre-cut-over test name (old `cloudflare.py` `UI_TEST_HOST`) | Live, but serves nothing different now (3.4) |
| 10 | **Staff allowlist** | `yyc/deploy/config.json:5-6` → tofu (`access.tf:56-60, 85-102`; workflow `.github/workflows/cloudflare.yml` triggers on this file, `:121-126`) and the router (ConfigMap, `router.mjs:591-596`) | Who can sign in, at both layers | Valid. A push applies both (the tofu run plus the ConfigMap hash rolling `ui`) |
| 11 | **iOS sign-in on/off**: `JARVIS_APP_SCHEME` | `YYC/yyc-ai/deploy/ui.yaml:84` (`dev.de0ch.yycai`); `router.mjs:621-622, 1074-1075` (unset → 404) | `/api/jarvis/app-signin` | Valid |
| 12 | **Access audience in config** | `yyc/deploy/config.json:4` (`access.teamDomain`, `access.audience`) must equal tofu output `aud.staff_ai` (`tofu/outputs.tf:99-108`) | Router rejects every token if they differ | Valid. Copied by hand; nothing checks it |
| 13 | **`access.also`** (second team) | `router.mjs:67-68, 322-345, 586-589`; test `jarvis/test/router.test.mjs:553-566` | Accepts tokens of more than one Access team | Code present, **unused**. Live ConfigMap `jarvis-config-tdc5d62c24` has only `yyc-hk` (3.5) |
| 14 | **Drift revert**: cloudflare.yml every 30 min | `.github/workflows/cloudflare.yml:127-128`; `tofu/main.tf:2-3` | Any switch flipped in the dashboard (an emergency offline, say) is undone within 30 min | Valid. An operator must flip in git, not in the dashboard |
| 15 | `UNDER_CONSTRUCTION` | was `yyc-ai/edge/worker.js:14` (`git show 29660229^:yyc-ai/edge/worker.js`) | 503 page 「YYC AI 正在升級」 / API 503 JSON | **Gone** (deleted 29660229) |
| 16 | `UI_CUTOVER` | was `yyc/infra/cloudflare.py:43-44` | tunnel origin `ui` vs `web` | **Gone**. Replaced by #3 |
| 17 | `cloudflare.py … --dns` | was `yyc/infra/cloudflare.py:19-24, 87` | `wrangler deploy` of `yyc-ai/edge`, DNS cleanup | **Gone** |
| 18 | `wrangler rollback` of `yyc-ai/edge` | skill `yyc-ai` SKILL.md:334 | Front-door rollback | **Gone** (Worker `yyc-ai` lived on Deyao's account `ee3b4dee…`. Whether the script still exists there could not be checked, because neither `CLOUDFLARE_API` nor `YYC_CF_API_TOKEN` is in this session) |
| 19 | Sandboxes off (not front door, but staff-visible) | skill `yyc-ai` SKILL.md:338 (`vm_hosts.max_sandboxes = 0`, `aws.maxHosts` 0) | Conversations can't start | Valid. Today it lives in hermit's tables |

### 3.1 The offline switch does not end existing sessions
`staff_ai` has `session_duration = 720h` (`access.tf:67-72, 170`). Swapping its policy to `admins` changes who Access
lets sign in. Cloudflare evaluates Access policy when a session is issued, so a staff member's existing
`CF_Authorization` cookie, or the token held in the iOS Keychain (720 h, `paseo/packages/app/src/yyc/access-token.ts:1-5`),
keeps passing cloudflared's aud check. The router then admits them because `config.json` `users` is unchanged
(`router.mjs:883`). This follows from Cloudflare's documented behaviour; I did not test it live. A real offline needs
one of these as well:
- Access's per-app session revoke (`POST /accounts/{acct}/access/apps/{id}/revoke_tokens`, done by hand; nothing in
  tofu or the repo does it);
- an empty answer to "who may use this deployment" from the backend.

`users` itself must not be emptied, because it is the shared catalogue for every YYC app (`access.tf:56-60`; README:119).
In the target design the second option is natural: hermit's "who may use the deployment" returns no one, and pigeon
refuses every request whatever token it carries, with no pigeon switch. The signed-out shell keeps showing 「登入」
in both cases, and sign-in then fails at Access. There is no "being upgraded" message today.

### 3.2 The old-UI rollback (README:120-123, jarvis3 docs/CLAUDE-CODE.md:194-201) is dead
- `web-from-ui` NetworkPolicy (`YYC/yyc-ai/deploy/ui.yaml:176-183`) admits only ui (and news-monitor) pods to `web`.
  An ingress pointed at `web` gets no answer from cloudflared.
- The Access app now covers only 5 paths and the tunnel passes the rest of the host with no token
  (`tunnels.tf:72`). The old web UI assumed Access on the whole host (old `cloudflare.py` docstring: "the Access app on
  the whole hostname"), so its pages would be public.
- jarvis3 has 43 commits since 2156d8a (2026-10-21 09:18). Postgres moved off Neon (lesson 82) and the host and team
  changed (2026-10-23/27). 2156d8a's config shape (`model` route, `s3`, web-env S3 keys) is no longer in YYC.
- After the magpie import, the old tables in `jarvis` matter even less. Under the clean-cut rule this runbook should be
  deleted along with the `ui` role, not carried forward.

### 3.3 yyc-edge is deployed by hand, outside GitOps
`news-stream/edge/deploy.sh:1-23` runs `wrangler@4 deploy` plus `secret put EDGE_SECRET` with `YYC_CF_API_TOKEN` and
`YYC_EDGE_SECRET` (store `yyc`). No workflow deploys it: `news-stream-image.yml:13` excludes `news-stream/edge/**`, and
no other workflow names it. Its smoke test (`deploy.sh:13-23`) checks the news stream's paths and the `/app` Access
gate, not the `MOVED` 301. Last changes: 94518cfe (added `MOVED`, 2026-10-27) and fe96fca7 (2026-10-29, MCP entry
removed). The YYC AI 301 is therefore one line inside another product's hand-deployed Worker. That conflicts with the
standing rule that GitOps runs in workflows, though the Worker belongs to the news stream, not YYC AI. `wrangler.toml:1-7`
holds account `00916611…` and no routes (the routes are tofu's).

### 3.4 Test host ai-yyc-ui.deyaochen.com
It was made so the CloudCLI UI could be tried behind the same Access app before the 2026-10-21 cut-over (old
`cloudflare.py` `UI_TEST_HOST`). It now routes to the same `ui` as the main name. It is still in three tofu places and
in Deyao's zone (`access.tf:64`, `saas.tf:31`, README record list, `monitor.py:1831`). For the switch to pigeon it
could serve the same purpose again (its tunnel rules → `pigeon`, main name still `ui`), or it should be removed. The
house rule is that new addresses go on `yyc-internal.com` (`saas.tf:5`), so a fresh test name for pigeon would be
`<x>.yyc-internal.com`, not this one.

### 3.5 access.also: what it was for
Added in claudecodeui 82a4caea (2026-10-23 03:50, "several Access teams (config access.also), the token's issuer picks
the team"). It was used in YYC 1ebe7c9e (2026-10-23 04:04, `"also": [{"teamDomain": "yyc-hk.cloudflareaccess.com",
...}]`) while YYC AI's Access moved from Deyao's account (team `de0ch`, old `cloudflare.py` `TEAM = "de0ch"`) to YYC's
account (team `yyc-hk`). Tokens from both teams' sessions validated during the move, each against its own issuer's
JWKS. It was removed 9 minutes later in 7584a958 ("Access checks accept only YYC's team yyc-hk"). It is a migration
aid, not a live feature. Under the target rules (no deployment-specific switches, clean cuts) pigeon should not carry
it over.

## 4. What a stateless pigeon must reproduce (Access contract)

Configured at Cloudflare (stays in tofu; pigeon depends on it):
- **Organisation** `tofu/access.tf:10-32`: `auth_domain yyc-hk.cloudflareaccess.com` (`main.tf:65`: `yyc` was taken),
  `strict_service_token_auth = true` (`:15`; accounts made after 2026-10-05 cannot turn it off, API error 12062),
  `allow_authenticate_via_warp = false`, `auto_redirect_to_identity = false` at org level (`:17`) and per app
  (`:71`). The login page therefore shows both IdP buttons instead of jumping to one. `deny_unmatched_requests = false`.
  `login_design` uses `logo_path https://ai.yyc-internal.com/yyc-logo.png` (`:29`), **so pigeon must serve
  `/yyc-logo.png` without sign-in**, or Access's login page loses its logo.
- **IdPs** `access.tf:34-54`: one-time PIN ("E-mailed code") always. Google only when `var.google_client_id` is set
  (`main.tf:42-50`; Actions secrets `YYC_CF_GOOGLE_CLIENT_*`, Google Cloud project `yyc-access-login`,
  `news-stream/deploy/README.md:157-158`). Access sends no code to an unlisted address but still says it did (skill
  `yyc-ai` SKILL.md:85).
- **App** `staff_ai` `access.tf:164-177`: `domain = ai.yyc-internal.com/api` plus destinations host × path, 720 h,
  http-only cookie, `app_launcher_visible = false`, policy `users` (precedence 1), `depends_on` the custom hostnames.
- **Tunnel enforcement** `tunnels.tf:65-73`: cloudflared refuses a signed-in path without a valid token of
  `staff_ai.aud`.

Done by the router today; pigeon has to do the same:
- **Token source** `router.mjs:204-210`: the `Cf-Access-Jwt-Assertion` header, else the `CF_Authorization` cookie
  (URI-decoded). The iOS app sends `cf-access-token` **and** `Cookie: CF_Authorization=` (`access-token.ts:47-53`).
  Access at the edge reads either. The router reads only the cookie or the assertion header, never `cf-access-token`.
- **Verification** `router.mjs:214-319`: RS256 only; JWKS from `https://<team>/cdn-cgi/access/certs`, cached 1 h,
  refetched for an unknown kid at most every 30 s, 5 s timeout. Checks: `iss == https://<team>`, `aud` contains the
  configured audience, `exp`/`nbf` ±5 s, and a non-empty `email` claim. Service tokens are refused ("service tokens are
  not staff", `:252`). First name comes from the claims, else `GET <team>/cdn-cgi/access/get-identity` (1.5 s, cached
  6 h / 5 min on failure, `:262-296`), for the greeting `/api/jarvis/me`.
- **Allowlist** `router.mjs:591-596, 883`: exact emails plus `@domain`. Otherwise an HTML 403 「沒有權限」.
- **Signed-out shell** `router.mjs:12-19, 953-956, 1093-1123`: no or invalid token on GET/HEAD outside
  `needsSignIn` returns a dist file (path-traversal checked, `/_expo/static/` and hashed names immutable, `.html`
  no-store) or `index.html` with `<title>` set to the product name and `<meta name="yyc-signed-out">`. Anything else
  gets 401 「未登入」.
- **Web sign-in** `router.mjs:1124-1134`: `GET /api/jarvis/signin?next=` (behind Access) returns 302 to a same-origin
  path that is not a signed-in path, else `/`.
- **Bare prefix redirect** `router.mjs:969-974`: GET/HEAD of exactly `/api`, `/api/`, `/ws`, `/paseo-ws` returns 302
  to `/`. This exists because Access sends a sign-in that started on its own login page back to the app's first
  destination, `/api` (`access.tf:167`).
- **iOS sign-in** `router.mjs:1066-1086`: `GET /api/jarvis/app-signin?state=` (state `^[A-Za-z0-9_-]{16,128}$`) returns
  302 to `dev.de0ch.yycai://signed-in?state=…#token=<CF_Authorization cookie, else assertion>`, with `no-store` and
  `no-referrer`. The app keeps the token 720 h and checks it at start with `GET /api/jarvis/me`. Access answers an
  expired or invalid token with a 302 to its HTML login (verified live), so the app treats "not JSON" as signed out
  (`yyc/access-gate.tsx:22-33`). A WS upgrade with a bad token also gets the 302 (verified), which the client sees as
  a failed upgrade.
- **Preflight/CORS**: none needed and none configured. The web build is same-origin (`yyc/site.ts:10-11`) and native
  iOS fetch/WebSocket send no CORS preflight. A browser preflight with `cf-access-token` is refused 403 by Access
  (verified). If pigeon ever serves a cross-origin client, the Access app would need `cors_headers` /
  `options_preflight_bypass` in tofu. Today it has neither.
- **Shipped iOS builds** have the origin frozen from the `YYC_SITE` build secret (`paseo/packages/app/app.config.js:179`;
  `.github/workflows/yyc-ios.yml:31`). The value is not visible. Every successful `yyc-ios` run (latest 2026-10-29
  9780d186) is after the 2026-10-27 move, but whether the secret was changed then cannot be checked here. A build
  pointing at `ai.yyc.deyaochen.com` gets a 301 on REST and a failed upgrade on `/paseo-ws`, because the Worker
  answers the WebSocket with a 301 (verified).

## 5. Consequences for the pigeon / magpie / hermit switch

1. **The cut-over is one tofu change**: `tunnels.tf:69,72` `ui.jarvis.svc` → `pigeon.jarvis.svc`, plus `ui-ingress` →
   a `pigeon-ingress` NetworkPolicy admitting cloudflared. cloudflared's remotely managed config reaches both
   connectors within seconds. Rolling back means reverting the commit. For zero downtime, `ui` and `pigeon` must both
   be serving while cloudflared switches. Sockets already open on `ui` stay until they close, and `ui` drains them
   (900 s, `router.mjs:36-39`). Only then is `ui` deleted (clean cut: Service, Deployment, `ui-peers`, NetworkPolicies,
   the `yyc-ai-ui` image pin, the `JARVIS_UI_*` env). The 30-minute drift revert (#14) means the switch must go through
   git.
2. **The signed-in path list shrinks, and must change in tofu and pigeon together.** `/ws` (CloudCLI's legacy socket:
   no YYC client) and `/__jarvis` (peer holds, which a stateless pigeon doesn't need, and the sent-files snapshot,
   which becomes an internal magpie REST call from egress and should never be on the public host) leave
   `access.tf:65`. `/ops` stays only if pigeon forwards it (today it is `web /internal/ops`, so hermit). The list is
   kept by hand in two repos today. The rewrite should make one of them generated, or at least add a test that
   compares pigeon's list against `access.tf`.
3. **Deployment-specific front-door settings vs the "no switches" rule.** Today the router reads YYC-specific values:
   Access team and audience (`config.json:4`), `users`/`admins`, `JARVIS_APP_SCHEME`, product name. The design says
   pigeon "asks hermit who may sign in" and no YYC-specific logic outside hermit/egress/tools. So the Cloudflare
   Access verification (team JWKS, aud, get-identity first name) and the allowlist are YYC behaviour, and the clean
   home is YYC's hermit. For example, pigeon hands hermit the request's credential and gets back {email, firstName,
   admin} or a refusal. The alternative is pigeon holding a YYC verifier, which would break the rule. The same
   question applies to the iOS `app-signin` redirect (it reads the Access cookie) and the signed-out shell's product
   name. This is noted here for whoever designs pigeon. Jarvis 4's side stays open as instructed.
4. **Offline becomes a hermit answer** (3.1). There is no `UNDER_CONSTRUCTION`-style page anywhere. If one is wanted
   for the switch window it would be a pigeon response to hermit refusing everyone, not a flag.
5. **The 301 stays where it is.** It belongs to the news stream's `yyc-edge` and is unaffected as long as YYC AI keeps
   `ai.yyc-internal.com`. Under the clean-cut rule, its eventual removal is two lines: `worker.js:17` `MOVED` and
   `saas.tf:30-31, 71`, plus Deyao's hand-set records.
6. **Public `/healthz` and `/readyz`** (live: process count readable by anyone) should not be carried into pigeon on
   the public host. Probes can use a port or path that the tunnel's second rule doesn't expose.
7. **`instance: "ai.yyc.deyaochen.com"`** in `config.json:27,34` is the AWS tag the scaler and janitor match
   (`yyc-ai/README.md` layout table; `jarvis3/chat/src/common/config.ts:260,294` derives it from `publicUrl` when
   absent), not an address. hermit inherits it. Renaming it orphans running hosts' tags unless they are retagged in
   the same change.
8. **Dormant e2b front door**: `egress.ai.yyc-internal.com` (`config.json:31-32`, `deploy/kustomization.yaml:91`) has
   no tofu record, tunnel rule or Access app yet (`yyc-ai/README.md:189-196`: "to add … when e2b is switched on").
   Under hermit it becomes a YYC hermit/egress detail, never a pigeon one.

## 6. Stale docs that name switches which no longer exist (fix in the refactor's clean cut)

- `jarvis3/docs/CLAUDE-CODE.md:194-201` "Rolling back the cut-over": `UI_CUTOVER`, `UNDER_CONSTRUCTION`,
  `cloudflare.py … --dns`. Also `:210-211`: "Neon databases cloudcli and jfs_yyc_ai" (they are on YYC's CNPG).
- `claude-env/.claude/skills/yyc-ai/SKILL.md:333-337` (Rollback: `wrangler rollback` of `yyc-ai/edge`,
  `UNDER_CONSTRUCTION`, `UI_CUTOVER`). Also `:83`: the allowlist step "re-run `cloudflare.py`"; tofu now applies on push
  of `config.json`. Also `:50`: "applied by tofu-controller on push"; it is the GitHub workflow `cloudflare.yml`
  since 2026-10-28 (`news-stream/deploy/README.md:153-156`). Also `:346`: "Front door/Access/tunnel: Deyao's Cloudflare
  free plan"; it is YYC's account now.
- `claude-env/.claude/skills/yyc-news-stream/SKILL.md:455`: "tunnel token → re-run `infra/cloudflare.py`" (deleted; the
  token is a tofu output, `tofu/outputs.tf:69-84`).
- `YYC/yyc-ai/README.md:118-123`: the offline step omits that existing sessions survive (3.1), and the old-UI step is
  dead (3.2).
- `YYC/news-stream/deploy/README.md:171`: says yyc-edge routes are "for yyc and yyc-beta"; `saas.tf:71` also routes
  `ai.yyc.deyaochen.com/*` (the README table at `:184` is correct).
