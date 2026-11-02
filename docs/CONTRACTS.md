# Contracts

For implementers of another hermit or another sandbox gateway. The contract crates are the text of record for the
types, route constants and protocol numbers; this page states the conventions around them.

Status: the conventions are complete; the rest is filled in with the contract crates: `hermit-api`, `supervisor-api` and `gateway-api` (the endpoint names
and the gateway contract), `magpie-api`, `sandboxd-api`.

## Conventions (every service)

`crates/aviary-core` implements these once; another implementation of hermit or of the gateway must follow them on
the wire.

### Ports, health, readiness, version

- `8080` serves the API (the Service maps port 80 to it); `9090` serves the ops routes and is never in a Service.
- `GET :9090/healthz` → `200 {"ok": true}`.
- `GET :9090/readyz` → `200 {"ok": true}`, or `503 {"ok": false, "why": [...]}` while the process has not finished
  its own start (each part a named gate, for example `"migrations"`) or while draining (`"draining"`, listed first).
  It never turns 503 because a dependency became unreachable after start.
- `GET :9090/version` → `{"name", "version" (12 hex), "revision" (40 hex), "builtAt" (RFC 3339), "provides":
  {surface: n}, "accepts": {surface: [n-1, n]}}`.
- `GET :9090/metrics` → Prometheus text 0.0.4, names `<svc>_*`, always with `<svc>_build_info{version,revision} 1`
  and `<svc>_draining` (0 or 1).
- `<bin> --version` prints `<name> <sha12> protocols={"provides":…,"accepts":…}`.

### Drain

After the Pod's `preStop` sleep, SIGTERM makes `/readyz` 503, then the service's drain steps run in order, each with
its own deadline: close listeners, end long-polls and streams with their "reconnect" answer, hand back leases and
leader locks, wait for in-flight work. The process then exits 0 (log `drained`). A second SIGTERM exits at once
(code 143).

### Paths and protocol numbers

Every service-to-service route is under `/v1/`; a breaking change adds `/v2/` beside it. Each surface has a protocol
number; clients send `Aviary-Protocol: <surface>=<n>` on every call; a server accepts its current and previous
number.

### Headers

Every header the services define starts with `Aviary-`: `Aviary-Request` (uuid v7 correlation id, minted at the edge
and forwarded unchanged), `Aviary-Protocol`, `Aviary-Person`, `Aviary-Viewer`, `Aviary-View-Session`, `Aviary-Admin`,
`Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation`, `Aviary-Sandbox-Person` (the gateway only), `Aviary-Lease`,
`Aviary-Upload-Name`, `Aviary-Upload-Kind`. A receiver removes every `Aviary-*` header the caller's role may not set
before reading any; a request from outside (a person's app, a sandbox) has all of them removed. Every answer carries
`aviary-request`.

### JSON and the error envelope

`content-type: application/json; charset=utf-8`; camelCase field and query names; unknown fields ignored; times RFC
3339 UTC with milliseconds; ids lowercase UUIDs; emails trimmed and lowercased before any comparison. Every non-2xx
answer is the flat envelope `{"code": "<lower_snake_case>", "message": "<English, for logs>", …fields}`: 400
`bad_request`, 401 `unauthenticated` (with `www-authenticate: Bearer`), 403 `wrong_caller` or a domain refusal, 404
`not_found`, 409 domain conflicts, 410 `cursor_expired`, 413 `too_large`, 415 `unsupported_type`, 429 `rate_limited`
and 503 `unavailable` (with `retry-after`), 507 `quota_exceeded`. Every answer has `cache-control: no-store` unless
the route says otherwise, and `x-content-type-options: nosniff`.

### Caller credentials

A pair token is 32 random bytes, base64url without padding. The caller sends `Authorization: Bearer <token>`. The
callee accepts a comma-separated list per caller (the current token first, two during a rotation) and compares
SHA-256 digests in constant time. Unknown or missing token: 401 `unauthenticated`; a valid token on a route its
caller may not use: 403 `wrong_caller` (the envelope names the `caller`).

### TLS and outbound HTTP

Plain HTTP inside the cluster; outbound HTTPS on rustls with the system trust store (`SSL_CERT_FILE` honoured).
Outbound calls send `User-Agent: <svc>/<sha12>`. Only idempotent calls (`GET`, `HEAD`, `PUT`, `DELETE`, `OPTIONS`, or
a body carrying the caller's idempotency key) are retried, with jittered backoff, on connection errors, timeouts,
429, 502, 503 and 504.

## Endpoint names

## The sandbox gateway contract

## The hermit contract
