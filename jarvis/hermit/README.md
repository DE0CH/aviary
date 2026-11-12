# aviary/: Jarvis 4's side of the aviary harness

Jarvis 4 dogfoods YYC AI (Deyao, 2026-11-11): a session with the harness `aviary` runs DE0CH/aviary's supervisor,
the same build YYC AI's sandboxes run, and its conversation lives in Jarvis 4's own pigeon and magpie, the same
builds as YYC AI's, at **https://chat.deyaochen.com**. Nothing in aviary is configured for Jarvis 4: it sees Jarvis 4
only behind the contracts (DE0CH/aviary docs/CONTRACTS.md), through this directory's hermit and gateway.

```
chat.deyaochen.com (Access "Jarvis 4 chat", Deyao)                     DE0CH/services k8s/chat, namespace services
  pigeon ─▶ magpie (Postgres chat-db, bucket deyao-chat-magpie)
     │         │
     └──────▶ hermit-jarvis ──(its service token: /api/remotes, /api/sessions/:id/start,
                 ▲   ▲          /api/machine-signatures/verify)──▶ Jarvis 4's router
                 │   └── chat-gw.deyaochen.com (Access "Jarvis 4 chat gateway", token aviary-gateway)
Jarvis 4 session machine (harness aviary)                                     │
  aviary-gateway 127.0.0.1:3128 ──── magpie/hermit, boot, health, deliveries ─┘
      ├─ model ─▶ api.anthropic.com with the session's current Claude login
      └─ internet (CONNECT, absolute-form HTTP)
  supervisor-keeper (PID 1 of its own PID + mount namespace: aviary-sandbox) ─▶ supervisor ─▶ Claude Code in /workspace
```

## Decisions (Deyao, 2026-11-11)

- Sessions start only in the Jarvis 4 app; one session is one conversation. The chat app offers no New chat (hermit's
  `/v1/deployment` `conversations.peopleCreate: false` → pigeon's `/api/jarvis/me` `newConversations: false`).
- Claude Code loads the session's workspace CLAUDE.md, skills and settings itself (the descriptor's
  `claude.projectSettings`: `--setting-sources=project`); YYC AI's hermit leaves it off.
- The model is the session's Claude subscription account.
  Anthropic takes a subscription login's call only when its system prompt begins with Claude Code's identity line,
  so aviary-gateway puts that line first on a call that is not Claude Code's own (the supervisor's title call:
  `machine.rs` `with_identity`).
- The conversation's title (magpie's: generated from the first message, or renamed in the chat) is the session's
  title on its card: hermit-jarvis reads it (`GET /v1/hermit/conversations/{id}`) and answers it to each health
  report, aviary-gateway serves it in `/jarvis/state`, and the harness's shim writes it as an `ai-title` line beside
  the mirrored transcripts (`aviary-title.jsonl`), where the machine's status reads a Claude session's title.
- hermit and the gateway are Rust, here; the gateway's sandbox-facing half runs on each session machine.
- pigeon and magpie are a DE0CH/services tenant.
- The session image's user `claude` is uid 1000 (the supervisor runs the agent as uid 1000).
- hermit learns which session a machine is from the router (the machine signs its boot; the router checks the
  signature for hermit), never from the machine's word.
- No iOS app: the web app only. The `cloudcli` harness and the Jarvis 4 Chat app are gone (a clean cut).

## Pieces

| Piece | Where | What |
|---|---|---|
| `hermit-jarvis` | `src/bin/hermit-jarvis.rs`, image `ghcr.io/de0ch/hermit-jarvis` | :8080 the hermit contract (`src/hermit.rs`), :8081 the gateway listener (`src/central.rs`), :8082 pigeon's models route, :9090 ops; the router as the sessions' truth (`src/reconcile.rs`, `src/jarvis.rs`) |
| `aviary-gateway` | `src/bin/aviary-gateway.rs` → `src/machine.rs`, in the session image | the machine's half of the gateway, the signed boot, the supervisor's health, deliveries |
| `harness-aviary.sh`, `aviary-sandbox` | `session-image/base/` | the harness launcher and the supervisor's namespace |
| supervisor, keeper | DE0CH/aviary release assets, pinned in `session-image/aviary.json` | |
| pigeon, magpie, pigeon-app | DE0CH/services `k8s/chat/` | pinned by DE0CH/services `chat-pin.yml` |
| router | `router/machinesig.go`, harness `aviary` in `router/harness.go`, `router/remote.go` | |

## A session's life

1. **Start** (the Jarvis 4 app, harness "Claude Code · aviary"): the policy adds the stores `tunnel` and `aviary`
   (`AVIARY_GW_ACCESS_ID/SECRET`). `harness-aviary.sh` starts `aviary-gateway`, which signs `POST /jarvis/boot`
   with the machine key (`jarvis-machine sign-request`). hermit asks the router whose signature it is
   (`POST /api/machine-signatures/verify`), makes the session's conversation in magpie once (magpie's
   `POST /v1/hermit/conversations`, idempotent by `session:<id>`), and a sandbox row for this boot. It answers the
   sandbox token (`hst1`, Ed25519, `HERMIT_TOKEN_KEYS`) and the control token. The same machine booting again (a
   gateway restart) keeps its sandbox.
2. **The supervisor** starts as root through `sudo aviary-sandbox <boot env>`: PID 1 of a new PID namespace, `/proc`
   its own (the memory guard sees only the agent's processes), `/home/sandbox` a bind of `/home/claude` with an empty
   tmpfs on `.claude` (magpie's FUSE goes there), `/workspace` a bind of `~/workspace`, `/etc/claude-code` a tmpfs.
   The network is the machine's: the agent reaches `$JARVIS_URL`, `session-secret`'s socket and the ssh agent as in
   any session. Its descriptor (through the gateway) carries the machine's non-secret Jarvis values
   (`src/wire.rs` `BOOT_ENV_NAMES`) and the proxy variables; no secret travels through hermit.
3. **Ready**: `aviary-gateway` reads the supervisor's `GET :7070/v1/health` every 2 s (the status shim's busy/idle)
   and reports it to hermit (`POST /jarvis/health`), which marks the sandbox `ready`.
4. **Turns**: from the chat (pigeon → magpie → the supervisor's command poll), or from Jarvis (the first prompt,
   wakeups, crons, watches, peer messages: `harness-send` → `POST 127.0.0.1:3128/jarvis/deliver` → hermit → magpie's
   `POST /v1/hermit/conversations/{id}/turns` as Deyao; 409 `turn_running` while a turn runs, which `harness-send`
   turns into 75 for the inbox loop to retry).
5. **Pause**: Jarvis 4 pauses an idle session itself (its idle hour); hermit's next pass over `/api/remotes?harness=
   aviary` ends that sandbox (`idle`) and closes the conversation to Deyao's messages (magpie's
   `PUT /v1/hermit/conversations/{id}/closed`): the chat's message box says the session is paused and takes nothing
   until the session is up again, when the same pass opens it (Deyao, 2026-11-12: a session is resumed in the
   Jarvis 4 app). A turn magpie admitted just before the close still resumes the session
   (`POST /api/sessions/:id/start`) and the new machine's boot takes the waiting sandbox; Jarvis's own wakeups and
   crons resume the session in the router, as for every harness. A destroyed session's
   conversation leaves the chat (`POST /v1/hermit/conversations/{id}/delete`); the chat offers no Delete and no
   Recently deleted (`conversations.peopleDelete: false`), since a session's life is managed in the Jarvis 4 app.
   `sessions.chat` records what magpie was last told (`open`, `closed`, `ended`). The conversation (Claude Code's `~/.claude`) is magpie's, so nothing of it rides in the snapshot; the
   harness copies its transcripts into `~/.claude/projects/-home-claude-workspace/` every 30 s for live sync,
   archives and transcript search.
6. **Release** (magpie's `release_sandbox`): hermit sets it pending; the supervisor gets it in its next activity reply
   (at most 15 s). hermit never calls into a machine.

## Its credential

hermit-jarvis reaches the router's device routes (`/api/remotes`, `/api/sessions/:id/start`,
`/api/machine-signatures/verify`, behind the Access app "Jarvis 4 devices") with its own Access service token
`hermit-jarvis` (DE0CH/services infra/cloudflare.py mints it; its values are `HERMIT_JARVIS_ACCESS_ID/SECRET` in the
Secret `chat-hermit`). The router admits it by client id, named in git (`HERMIT_CLIENT_ID` in
k8s/cluster/router/router.yaml), as it admits the health probe's: no browser login, and removing the line (or the token
at Cloudflare) cuts it off.

## Bumping aviary (dogfooding)

`aviary-pin.yml` (every 15 min) pins DE0CH/aviary's newest release into `session-image/aviary.json` and this crate's
`Cargo.toml`; DE0CH/services `chat-pin.yml` pins the same release's pigeon and magpie, the newest pigeon-app and
hermit-jarvis. A release exists only when aviary's CI passed, so every green aviary commit reaches Jarvis 4 within
the hour, before YYC AI (whose pins move by hand).

## Developing

- `./dev-patch.sh ~/workspace/aviary test` builds and tests against a local DE0CH/aviary checkout (before an aviary
  change is pushed). `HERMIT_TEST_DATABASE_URL`: a TLS Postgres server (aviary-pg requires TLS); `tests/flow.rs`
  runs the whole life above against it, aviary's FakeMagpie and a stand-in router.
- Pitfall: `reqwest` here has no default TLS provider (`rustls-no-provider`, as in aviary): every client takes
  `aviary_core::http::tls_config()`, or it panics at build.

## End to end, with no Inbox: `infra/e2e-aviary.sh`

The whole harness on a replica, run from a session (needs `JARVIS2_FLY_TOKEN`, store `infra`; sudo):
`session-secret --keys JARVIS2_FLY_TOKEN --exec infra/e2e-aviary.sh` (about 6 minutes; logs in
`/tmp/jarvis4-e2e-aviary-logs`). A throwaway kernel line and router on the session (the TEST master key, a root key
of its own, e2e/aviary playing the iPhone: roster, Reset, unlock, approval), the chat stack beside them (this tree's
hermit-jarvis, magpie of the pinned aviary release from its image, a TLS Postgres, `rclone serve s3`), a stand-in
model (Claude Code's calls arrive with the account's token as on a real machine), and a real Fly machine on the
current session image. Checked: the machine boots a sandbox through hermit-jarvis, the supervisor runs Claude Code on
magpie's ~/.claude, the first prompt arrives, a Bash tool call runs in the sandbox and its output comes back in the
answer, the wakeup that command armed through the session's Jarvis API arrives as a later turn (all read back
through magpie's admin API), and destroy burns the source. Nothing of production's is touched; the machine is
destroyed with its volume at the end (or on Ctrl-C).

- The test machine reaches the run over Fly's private network straight to the session's own 6PN address; off Fly the
  script makes two WireGuard peers of the org served by `router m-gateway` (that path did not connect from a Fly
  machine, 2026-11-11).
- The session image under test is `ghcr.io/de0ch/jarvis4-session:latest` (`E2E_SESSION_IMAGE` for another): a
  harness change is tested once images.yml has built it. hermit-jarvis is always this tree's.
- `E2E_ONLY=chat` brings up only the chat stack and keeps it (debugging hermit-jarvis against a real magpie).
- The test store `aviary` names the run's chat front (`AVIARY_GW_URL`) and the stand-in model (`AVIARY_MODEL_URL`):
  the same keys a production store leaves unset (chat-gw.deyaochen.com, api.anthropic.com).
