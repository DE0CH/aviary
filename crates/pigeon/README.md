# pigeon

The stateless gateway the web and iOS apps talk to (pigeon spec, `refactor/spec-pigeon.md`). `pigeon serve` runs it;
`pigeon --version` prints the build. Image: `images/pigeon/Dockerfile` (`ghcr.io/de0ch/pigeon:<sha12>`).

Environment: secrets only (spec 3.3) — `PIGEON_TOKEN_MAGPIE`, `PIGEON_TOKEN_HERMIT`, `PIGEON_TOKEN_MODEL`,
`PIGEON_VIEW_AS_KEYS`, `PIGEON_BLOB_KEYS` — plus `POD_NAME` from the downward API (`aviary_core::log::init_in_pod`).
Upstreams are the fixed names `http://magpie`, `http://hermit`, `http://model` and `https://<team domain>`
(`Upstreams::cluster`); the app bundle is read from `/srv/app`.

## Modules

| Module | Spec | What |
|---|---|---|
| `identity` | 4 | deployment facts (H1, every 60 s), Access token sources and RS256 verification against every team's JWKS (1 h cache, unknown-kid refetch at most every 30 s), verified-token cache, hermit people answers (15 s, stale-on-error 300 s), first names, the request identity |
| `http` | 5.1, 5.2.3, 5.5, 5.6, 5.13 | the routing table in order, path normalisation, the apps' error shape, the pages pigeon writes, same-origin rules, the model list, warm hints |
| `static_files`, `shell` | 5.2 | the bundle (in-memory file list, `.br`/`.gz` siblings, cache rules) and the rewritten `index.html` |
| `signin`, `viewas`, `ops`, `uploads`, `blobs`, `shares` | 5.3-5.4, 6, 5.11, 5.7, 5.8-5.9, 5.10 | the REST routes beside the socket |
| `server` | 5.13, 11.2 | the 8080 listener: connection tracking, `connection: close` while draining, the transfer bound |
| `paseo::compat` | 1.2 | the shipped clients' wire constants: the only file in the crate that may name them (source rule, platform 4.5) |
| `paseo` | 7 | the seams the socket package fills: `SocketHandler` (`/paseo-ws`) and `RowProjector` (spec 7.8; also used by the share view) |

## Seams for the socket package (WP-17)

`App::with_socket(handler, projector)` installs the `/paseo-ws` handler and the row projection. Until then
`paseo::NoSocket` answers every authorised upgrade with 503 and `paseo::BasicProjector` projects every row kind of
7.8 without the ported tool-call mapper (a tool row's `detail` is the mapper's `unknown` kind) and without the 7.9
sentences (error rows show magpie's `message`). The socket's drain step (11.2 step 3, sockets closed with 1012 and
jitter) is a drain hook the socket package adds before the listener's.

## Deviations (recorded)

- **Header-read and idle timeouts** (spec 5.13: 30 s / 120 s): hyper 1.12 runs its header-read timer whenever a
  connection waits for the next request head, idle keep-alive included, so the two cannot be set apart. pigeon sets
  one 120 s bound, the one that matters (cloudflared reuses a pooled connection for up to 90 s).
- **Share view streaming** (spec 5.10): magpie's answer is parsed whole (`magpie_api` has no row-by-row reader);
  the projection is streamed out row by row, so the projected timeline is never held whole.
- **Byte relays**: `magpie-api` gained `files_raw_relay`, `share_files_raw_relay` and `blob_relay` (the range and
  conditional headers go up, every status comes back), so 206/304/416 pass through as spec 5.8 says.
- **Cross-origin refusal body**: `{"success": false, "error": {"code": "CROSS_ORIGIN", …}}`, the apps' error shape of
  5.1, rather than 5.13's bare `{"code": "CROSS_ORIGIN"}`.
- **View-as with no valid sign-in**: a request whose own Access token is missing or invalid is `SignedOut` (401)
  even when it carries a view-as token; 6.2's refusal (`VIEW_AS_REFUSED`) applies to a signed-in caller.
- **Legacy view-as tokens** (`PIGEON_LEGACY_VIEW_AS_KEY`, spec 6.2, DESIGN 9.5) are WP-50's (`src/legacy/`); the
  7.10 migration retry is not built (DESIGN 12 D14, D24).
- **Drain test at CI scale** (spec 18.3): the upload across a drain is 4 MiB at about 2 MiB/s with the drain
  0.5 s in, not 50 MiB at 2 Mbit/s (210 s).

## Tests

- `tests/http.rs`: spec 18.2 (Access, people, shell, routing, sign-in, view-as, ops, uploads, files and blobs,
  shares, same origin) against fake-hermit, fake-magpie, a model stub and two fake Access teams (keys from `openssl`
  per run).
- `tests/drain.rs`: readiness gates and spec 18.3's HTTP drain cases.
