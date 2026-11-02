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
- the hashed files (`_expo/static/**`, `assets/**`) of the last 5 pigeon-app builds, of every build of the last 30
  days (pigeon spec 17.1), and of every pigeon-app build YYC served within the 720 h Access session lifetime (its pin
  history), merged in from the previous image (`:main`);
- the hashed files of **every** `yyc-ai-ui` image served within the 720 h Access session lifetime. That list is the ui
  pin history in DE0CH/YYC's git log (`pins.mjs`). While `jarvis-image.yml` still exists, the ui image of the same
  commit is added too. At A1 that is the image YYC is about to pin, and pigeon-app stays at that commit until A5. The
  files are copied out of the images themselves (`fetch-seeds.mjs`), so builds do not need to be deterministic. When
  a tag was re-pushed (a re-run of jarvis-image), the files of its earlier digest stay too.

A seed leaves when its image has not been served for 720 h, and an earlier build when it is none of the above: a tab
loaded from an image last served more than 720 h ago holds no live Access session. A hashed name that holds different
bytes in two of these sources fails the build, because no single file could serve both tabs. `verify-seeds.mjs`
checks the assembled tree before the push, and the pushed image after it: every seeded file must be present byte for
byte.

## Size

pigeon's emptyDir is 512 MiB (pigeon spec 17.1). The assembled `/app` may take 480 MiB of it, counted as the filesystem
stores it (4 KiB blocks); `assemble.mjs` fails over that and prints what takes the space. To stay well under it:

- `.br`/`.gz` siblings are written for the pigeon-app builds' files and the unhashed files only. Files kept only for
  seeds are served as they are (pigeon serves the file itself when no sibling exists, pigeon spec 5.2.1). Those are the
  old ui builds' chunks, mostly 21.5 MB entry bundles from before the chunk split, which a running tab never fetches
  again. This saves 110 MiB on the first image.
- The first image (`0b3a5c6f5028`, 39 seeds) took 429.7 MiB with the seeds' siblings; assembled the current way it
  takes 319 MiB (checked against that image). A build whose every file changed adds about 36 MiB. The test
  "assemble at the real sizes" runs two builds at the first image's real file sizes and checks that the last 5 builds,
  every file changed in each and all 39 seeds still listed, fit (464 MiB).
- Everything kept ages out by itself (seeds 720 h after their last serving, builds after 30 days when not among the
  last 5 or still served), so a build that is over the budget fits on a later run without a change. To ship sooner,
  the lever is the emptyDir `sizeLimit` and this budget.

## Files

| File | What |
|---|---|
| `pins.mjs` | the tags of one image (`yyc-ai-ui`, `pigeon-app`) served within a window, from a deploy repository's first-parent history of `yyc-ai/deploy` (any `kustomization.yaml` under it, so the `legacy/` move is followed) |
| `fetch-seeds.mjs` | `/app/dist/_expo/static/**` and `/app/dist/assets/**` of each ui image, via `crane export`. The CI cache is a download cache only: a cached tag is used only at the tag's current digest and with exactly the files and sha256 that the previous image's `meta/seeds.json` records (any job of the branch can write a cache entry, the build job included) |
| `assemble.mjs` | the new export + `sw.js` + kept and seeded files, pruning, `.br` (q11) and `.gz` (9) siblings (reused when the bytes are unchanged), the size budget; writes `meta/assets-history.json` (`{file: firstSeenISO}`), `meta/builds.json` and `meta/seeds.json` |
| `verify-seeds.mjs` | every seeded file present byte for byte |
| `check-frames.sh` | the contract check against pigeon: pigeon's frame corpus (`pigeon-frames.ndjson` of the newest `aviary-*` release), plus the harness's bridge and pigeon-only fixtures, validated against this checkout's `paseo/packages/protocol` with aviary's `testing/paseo-contract` harness. Until aviary publishes a release, it checks the fixtures of aviary main and prints a warning |
| `unpack-harness.sh` | the release's `paseo-contract.tgz`, with the harness at its root or in one top directory (npm pack's `package/`) |
| `self-test-verdict.sh` | the frame check's self-test passes only when the validator ran and rejected the seeded frame and nothing else |
| `clients-missing.mjs` | the dispatched commits aviary's `clients.txt` does not list yet |
| `Dockerfile` | the image; build context = the assembled directory (`app/`, `meta/`). `/meta` is not copied into `/srv/app`, so it is not served |
| `test/` | `node --test scripts/pigeon-app/test/*.test.mjs`; `incompatible-frame.ndjson` is the workflow's self-test of the frame check; `sizes-0b3a5c6f5028.json` the file sizes of the first image |

## The workflow

1. **build** (no secrets, read-only token: it runs the app's dependencies): the scripts' tests, `npm --prefix paseo
   ci`, the `@expo/metro-config` patch assertion, `scripts/build-paseo-ui.mjs`, `scripts/check-chunks.mjs --strict`
   (the budgets), the frame check, and the frame check's self-test.
2. **image** (one at a time; runs no dependency code): reads the pin history with `YYC_PIN_HISTORY_KEY`, a read-only
   deploy key of DE0CH/YYC. Then it exports `:main`, fetches the seeds (checking any cached one against `:main`'s
   record), assembles (with the budget), pushes `:<commit 12>`, checks the pushed image and that `/cp` works as uid
   65532 on a read-only root, and moves `:main` (also after a run of an older commit that waited behind a newer one:
   its image holds everything the newer one kept). Finally it sends ONE `paseo-client` dispatch to DE0CH/aviary with
   `AVIARY_DISPATCH_TOKEN` (a fine-grained token for DE0CH/aviary only, Contents read and write): this commit and
   every seeded ui commit, as `web`. aviary's `clients.yml` lists the pre-Paseo ones as `web-legacy`, and re-listing a
   commit is a no-op. The job then waits (up to 60 min) until aviary main's `clients.txt` lists every dispatched
   commit, and fails if it does not: a dispatch is only queued work.

GitHub keeps one pending run per concurrency group, so of three quick pushes the middle one's image job is cancelled.
That commit gets no pigeon-app image (nothing can pin it) and is not dispatched. If YYC pins its ui image, the next
pigeon-app build seeds and lists it, since the seed list is read anew on every run; the A1 build reads all 720 h of
pins, so by A1 every ui commit a pigeon-served tab can run is in `clients.txt`.

The package is public (platform 5.5). After its first push, someone with the account's web login sets the visibility
once (claude-env `github-web` skill, `package-public pigeon-app`).

## The A1-A5 freeze

From A1 to A5, YYC keeps `pigeon-app` at the A1 commit. Only a fix that changes nothing but pigeon-app may ship in
that window (DESIGN 9.6). Nothing enforces this yet: cutover's `freeze-check` (WP-35) refuses `ui.yaml`,
`jarvis-config`, the `legacy/` `secrets-rev` annotations, `db.yaml` and the old connectors' manifests, but not the
`pigeon-app` `newTag` in `yyc-ai/deploy/kustomization.yaml`. That check belongs in `freeze-check`: a `pigeon-app` tag
change between A1 and A5 is allowed only to a claudecodeui commit whose diff from the A1 commit touches nothing outside
`scripts/pigeon-app/` and `.github/workflows/pigeon-app.yml`.
