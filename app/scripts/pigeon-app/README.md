# pigeon-app

The Paseo web app as an image for pigeon, DE0CH/aviary's front service. `.github/workflows/pigeon-app.yml` builds
`ghcr.io/de0ch/pigeon-app:<commit 12>` on every push to main that touches the app (`paseo/**`, `public/sw.js`, the build
scripts). The image is `FROM scratch`: the files sit under `/app`, plus a static `/cp` (busybox's cp). pigeon's
initContainer runs `/cp -a /app/. /srv/app/` into the emptyDir that pigeon serves. DE0CH/YYC pins the tag by hand,
next to pigeon's own tag. The workflow deploys nothing.

## Why the image holds old chunks

A browser tab keeps running the JavaScript it loaded, and it fetches its lazy chunks by hashed name long after a newer
build is deployed. So each image holds:

- the new export;
- the hashed files (`_expo/static/**`, `assets/**`) of the last 5 pigeon-app builds and of every build of the last
  30 days, merged in from the previous image (`:main`);
- the hashed files of **every** `yyc-ai-ui` image served within the 720 h Access session lifetime. That list is the ui
  pin history in DE0CH/YYC's git log (`ui-pins.mjs`). While `jarvis-image.yml` still exists, the ui image of the same
  commit is added too. At A1 that is the image YYC is about to pin, and pigeon-app stays at that commit until A5. The
  files are copied out of the images themselves (`fetch-seeds.sh`), so builds do not need to be deterministic.

A hashed name that holds different bytes in two of these sources fails the build, because no single file could serve
both tabs. `verify-seeds.mjs` checks the assembled tree before the push, and the pushed image after it: every seeded
file must be present byte for byte.

## Files

| File | What |
|---|---|
| `ui-pins.mjs` | the `yyc-ai-ui` tags served within a window, from a deploy repository's first-parent history of `yyc-ai/deploy` (any `kustomization.yaml` under it, so the `legacy/` move is followed) |
| `fetch-seeds.sh` | `/app/dist/_expo/static/**` and `/app/dist/assets/**` of each ui image, via `crane export` (cached per tag in CI) |
| `assemble.mjs` | the new export + `sw.js` + kept and seeded files, pruning, and `.br` (q11) and `.gz` (9) siblings (reused when the bytes are unchanged); writes `meta/assets-history.json` (`{file: firstSeenISO}`), `meta/builds.json` and `meta/seeds.json` |
| `verify-seeds.mjs` | every seeded file present byte for byte |
| `check-frames.sh` | the contract check against pigeon: pigeon's frame corpus (`pigeon-frames.ndjson` of the newest `aviary-*` release), plus the harness's bridge and pigeon-only fixtures, validated against this checkout's `paseo/packages/protocol` with aviary's `testing/paseo-contract` harness. Until aviary publishes a release, it checks the fixtures of aviary main and prints a warning |
| `Dockerfile` | the image; build context = the assembled directory (`app/`, `meta/`). `/meta` is not copied into `/srv/app`, so it is not served |
| `test/` | `node --test scripts/pigeon-app/test/*.test.mjs`; `incompatible-frame.ndjson` is the workflow's self-test of the frame check |

## The workflow

1. **build** (no secrets, read-only token: it runs the app's dependencies): `npm --prefix paseo ci`, the
   `@expo/metro-config` patch assertion, `scripts/build-paseo-ui.mjs`, `scripts/check-chunks.mjs --strict` (the
   budgets), the frame check, and the frame check's self-test (it must fail on the seeded frame).
2. **image** (one at a time; runs no dependency code): reads the ui pin history with `YYC_PIN_HISTORY_KEY`, a
   read-only deploy key of DE0CH/YYC. Then it fetches the seeds, merges with `:main`, assembles, checks the 448 MiB
   budget (pigeon's emptyDir is 512 MiB), pushes `:<commit 12>`, checks the pushed image and that `/cp` works as uid
   65532 on a read-only root, and moves `:main`. A run of an older commit that waited behind a newer one leaves
   `:main` alone. Finally it sends ONE `paseo-client` dispatch to DE0CH/aviary with `AVIARY_DISPATCH_TOKEN` (a
   fine-grained token for DE0CH/aviary only, Contents read and write): this commit and every seeded ui commit, as
   `web`. aviary's `clients.yml` lists the pre-Paseo ones as `web-legacy`, and re-listing a commit is a no-op.

The package is public (platform 5.5). After its first push, someone with the account's web login sets the visibility
once (claude-env `github-web` skill, `package-public pigeon-app`).

From A1 to A5, YYC keeps `pigeon-app` at the A1 commit. Only a fix that changes nothing but pigeon-app may ship in
that window (DESIGN 9.6).
