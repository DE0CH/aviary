# aviary-conformance and test-gateway (WP-09)

`aviary-conformance` checks an implementation of an aviary contract from the outside (platform spec 8);
`test-gateway` is the reference implementation of the sandbox gateway contract (platform 3.11, 7.3) and the oracle
the gateway suite is tested against. Both are in the `ghcr.io/de0ch/aviary-testing:<sha12>` image
(`images/aviary-testing`; the container's command names the binary).

```
aviary-conformance hermit  --target http://hermit --tokens tokens.json [--capabilities pause,snapshot]
                           [--token-command '<prints the token of {sandboxId} {conversationId}>']
aviary-conformance gateway --proxy http://gw:3128 --api http://gw:8082 --sandbox-ip <source address>
                           --serve-fakes :80 --tokens tokens.json --signing-key <kid:base64url | @file>
                           [--sigterm '<sends SIGTERM to the replica under test>'] [--self-host <vip or name>]…
aviary-conformance model   --target http://model --token <pigeon pair token>
                           [--proxy http://gw:3128 --sandbox-token <token>]
common: --junit <file> (JUnit XML), --only <check-name prefix> (repeatable)
exit code 0 = no check failed; one line per check on stdout (PASS / FAIL / SKIP)
```

## The gateway suite

The suite serves fakes of `magpie`, `hermit`, `tools` and the model upstream on one listener that answers by `Host`
(`magpie`, `hermit`, `tools`; any other host is the model upstream), so the gateway's upstreams must reach
`--serve-fakes` (Docker network aliases, Services, or local addresses with the gateway sending `Host: <name>`). The
fake hermit publishes the public half of `--signing-key` and answers liveness as each check sets it; the suite mints
its own sandbox tokens with that key, presented from `--sandbox-ip`.

`tokens.json`: `{"gatewayToMagpie", "gatewayToHermit", "gatewayToTools", "gatewayToModel", "pigeonToGateway",
"hermitToGateway"}`: the pair tokens the gateway presents to the fakes, the model credential it presents upstream
(`x-api-key` or `Authorization: Bearer`), and the bearers its in-cluster API accepts.

Phases: a warm-up until a valid token is admitted (up to 60 s, so the suite and the gateway may start in either
order); every check that needs hermit up, concurrently; hermit's outage (3 min: connections dropped, answers held past
the gateway's 1 s timeout, 503); the drain (`--sigterm`), which ends the replica. Checks, by name:

| Check | Platform 8.2 row |
|---|---|
| `credential.missing`, `credential.not-sbx`, `credential.bad-signature`, `credential.expired`, `credential.wrong-source`, `credential.unknown-kid` | 407 / 403 for a missing, foreign, forged, expired, misplaced or unknown-kid credential, never forwarded |
| `credential.new-kid` | a key published after the gateway's last fetch is learnt on the first token naming it (the refetch of an unknown kid) |
| `liveness.revoked`, `liveness.unknown-sandbox` | `live: false` → 403 within 30 s; hermit's 404 → 403 |
| `liveness.hermit-outage` | a token seen before and one never seen are admitted on every request for the whole outage |
| `route.magpie`, `route.magpie-request-id`, `route.hermit` | the `Aviary-Sandbox-*` triple from the token (person lowercased), the callee's pair bearer alone, no client `Authorization`, `x-api-key`, `Proxy-Authorization` or `Aviary-*` (but `Aviary-Protocol` and `Aviary-Lease`, which arrive unchanged; see below), a valid lowercase v7 `Aviary-Request` kept, an invalid one replaced |
| `route.magpie-not-sandbox`, `route.hermit` | non-sandbox paths (and dot or encoded-dot paths) 403, not forwarded |
| `route.model-stream` | the gateway's model credential, never the sandbox's; the SSE body byte-identical; first data byte within 200 ms of the upstream's; 30 s unbroken |
| `route.model-messages`, `route.models`, `api.models` | messages and `count_tokens` relayed unchanged; the R18 models shape through the proxy and in-cluster with pigeon's bearer (401 without) |
| `route.tools` | a tools answer after 170 s relayed, with the tools pair bearer and `Aviary-Sandbox-Person` |
| `transport.long-poll` | a 25 s magpie long-poll not cut |
| `limits.64mib`, `limits.64mib-plus-one`, `limits.upload-cap` | 64 MiB delivered, 64 MiB + 1 → 413 `too_large`, 40 × 1 MiB on one kept-alive connection all delivered |
| `self-host` | a `--self-host` is neither refused like an internet host nor forwarded (skipped without `--self-host`) |
| `internet.denied` | an unlisted host and `CONNECT` refused, not forwarded |
| `api.ops-summary` | platform 15.3's shape with hermit's bearer (owners are emails), 401 without |
| `drain` | with a 60 s model stream, a 25 s long-poll and an idle kept-alive connection open: the stream completes, the long-poll answers with `Connection: close`, the idle connection is closed, a new connection is refused (skipped without `--sigterm`) |

`--quick` (hidden) shortens the slow checks for the suite's own tests; a pass with it proves nothing.

## The hermit suite

hermit spec 24.1 through hermit's routes with the callers' pair tokens (`tokens.json`: `{"pigeon", "magpie",
"gateway", "tools", "monitor", "admin"}`, each optional; a check needing a missing one is skipped). It needs a
catalogue that allows somebody (it reads `GET /v1/people`). Checks: `auth.*` (401 with `www-authenticate`, 403
`wrong_caller`, 400 without `Aviary-Protocol`), `deployment`, `people.*` (explicit entries, `@domain`, no subdomain
match, admin implies allowed, case and whitespace, `bad_email`), `warm`, `sandbox.*` (bad uuid, not allowed,
`no_sandbox`, two PUTs at once give one sandbox, the long-poll's timeout and wake, `person_mismatch`, pause / resume /
snapshot working or 501 `not_supported` as `--capabilities` says), `sandbox-surface` (the descriptor with matching
headers only, `ETag` and 304, 403 / 401 / 404 for the wrong caller, no token, another conversation or person, secrets
404 `not_granted`, the activity reply, busy reflected), `tokens` (published Ed25519 keys, liveness, 404
`unknown_sandbox`, a sandbox's token verifying with the keys: needs `--token-command`), `workspaces`,
`release-and-feed` (release idempotent, the sandbox gone and not live, the feed in seq order ending `gone`, `limit`,
a wait timing out empty), `feed.retention` (410 `cursor_expired`), and `json.camel-case` over every JSON answer.

## The model suite

Platform 8.3: the R18 models route, a Messages API round trip (JSON), the SSE event sequence of a streamed one, and
`count_tokens`. With `--proxy` and `--sandbox-token` the Messages API calls go through a gateway as `http://model/…`.

## test-gateway

`test-gateway [--proxy-listen 0.0.0.0:3128] [--api-listen 0.0.0.0:8082] [--ops-listen 0.0.0.0:9090] [--magpie
http://magpie] [--hermit http://hermit] [--tools http://tools] [--model http://mock-model] [--self-host <h>]…
[--ops-summary <json>]`, with the credentials from the environment: `GATEWAY_TOKEN_MAGPIE`, `GATEWAY_TOKEN_HERMIT`,
`GATEWAY_TOKEN_TOOLS`, `GATEWAY_MODEL_KEY` (sent as `x-api-key`), `GATEWAY_ACCEPT_PIGEON`, `GATEWAY_ACCEPT_HERMIT`.
`/readyz` is 503 `token-keys` until hermit's key set has been fetched once. A self host serves no local paths (404);
`CONNECT` and every other host are 403 `not_allowed` (the harness has no internet). Upstream requests carry `Host:
<fixed name>` for `magpie`, `hermit` and `tools`. The library's `Faults` seed four mistakes (client `Aviary-*`
headers kept, a failed liveness lookup refusing, the upload cap counting fixed names, a drain cutting streams); the
binary never sets them, and `tests/gateway.rs` checks that the suite fails each.

## Where it runs

- `ci.yml` `test`: `tests/gateway.rs` (the suite passes against test-gateway in-process and fails on each seeded
  fault), `tests/hermit.rs` (fake-hermit with every hook: nothing skipped), `tests/model.rs` (mock-model directly and
  through test-gateway's proxy with fake-hermit).
- `ci.yml` `conformance`: `.github/ci/conformance.sh`, the static binaries with the contract's own durations; JUnit
  XML and logs in the `conformance` artifact.
- jarvis3's `chat.yml`: `gateway` and `model` against the egress image under test (platform 17.3).

## Deviations from the specs (recorded for the design's owners)

- **`Aviary-Protocol` is forwarded.** Platform 3.11.3 says the gateway strips every client `Aviary-*` header, but
  hermit and magpie refuse a call without `Aviary-Protocol` (400 `unsupported_protocol`, platform 3.4) and the
  supervisor sends it through the gateway; it names no identity. `gateway_api::headers::FORWARDED` keeps it, and
  `route.magpie` checks that it arrives.
- **`Aviary-Lease` is forwarded.** For the same 3.11.3 sentence: platform 3.5 lists `Aviary-Lease` as set by the
  supervisor, and its rule is to strip only the headers the caller role may not set. magpie requires the lease on
  every state-changing `/v1/sandbox/**` call and on the command poll (magpie 3.3, 6.9), and R32 sends all of those
  through the gateway, so a gateway that strips it makes magpie refuse every supervisor write. The lease grants no
  identity: magpie checks it against hermit's current sandbox. `headers::FORWARDED` keeps it, and `route.magpie`
  sends one and requires it to reach magpie unchanged.
- **A new request on a kept-alive connection during the drain is served** with `Connection: close` (3.11.6 step 2),
  not refused with 503 `unavailable` (3.11.5's last sentence): the drain section is the specific rule.
- **"The next request on the busy keep-alive connection carries `Connection: close`"** is checked as the answer to the
  request that was in flight on it at the signal: HTTP/1.1 has no next request on a connection whose answer is still
  coming, and after that answer the connection closes.
- **Unknown kid "after one refetch"**: the gateway refetches at most every 10 s, so a refetch cannot be required at
  an arbitrary moment; `credential.unknown-kid` checks the 403 and `credential.new-kid` checks the refetch (a newly
  published kid is admitted).
- **hermit's 410 after retention** cannot be forced through hermit's routes (7 days); `feed.retention` uses the
  `expire_feed` hook in-process and otherwise passes on a 410 or is skipped.
- **`liveness live:false for not-allowed`** needs a catalogue change, which no route makes; it is covered by hermit's
  own integration tests (hermit 24.2), not by this suite.
