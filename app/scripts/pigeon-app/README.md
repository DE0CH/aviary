# pigeon-app

The Paseo web app as an image for pigeon, DE0CH/aviary's front service. `.github/workflows/pigeon-app.yml` builds
`ghcr.io/de0ch/pigeon-app:<commit 12>` on every push to main that touches the app (`paseo/**`, `public/sw.js`, the build
scripts) or, while `jarvis-image.yml` builds the ui image, what the ui image ships beyond it (`jarvis/**`, `server/**`,
`shared/**`, `public/**`, `package.json`, `package-lock.json`). The image is `FROM scratch`: the files sit under `/app`,
plus a static `/cp` (busybox's cp). pigeon's initContainer runs `/cp -a /app/. /srv/app/` into the emptyDir that pigeon serves. DE0CH/YYC pins the tag by hand,
next to pigeon's own tag. The workflow deploys nothing.

## Why the image holds old chunks

A browser tab keeps running the JavaScript it loaded, and it fetches its lazy chunks by hashed name long after a newer
build is deployed. So each image holds:

- the new export;
- the hashed files (`_expo/static/**`, `assets/**`) of every pigeon-app build YYC served within the 720 h Access
  session lifetime (its pin history), and, as far as the size budget allows, of the last 5 builds and every build of
  the last 30 days (pigeon spec 17.1), merged in from the previous image (`:main`). A served build that `:main` no
  longer holds (a rollback to a pruned build) is restored from its own image `pigeon-app:<tag>`;
- the hashed files of **every** `yyc-ai-ui` image served within the 720 h Access session lifetime. That list is the ui
  pin history in DE0CH/YYC's git log (`pins.mjs`). While `jarvis-image.yml` still exists, the ui image of the same
  commit is added too. At A1 that is the image YYC is about to pin, and pigeon-app stays at that commit until A5
  (below, "The A1-A5 freeze"). The
  files are copied out of the images themselves (`fetch-seeds.mjs`), so builds do not need to be deterministic. When
  a tag was re-pushed (a re-run of jarvis-image), the files of its earlier digest stay too.

A seed leaves when its image has not been served for 720 h, and an earlier build when it is none of the above: a tab
loaded from an image last served more than 720 h ago holds no live Access session. "Served within 720 h" is read from
committer dates, which are not deploy times, so `pins.mjs` adds a 24 h margin, ends a pin no earlier than the later of
its own commit's date and its replacement's, and reads the whole log rather than stopping at the first old date. Once
the switch's cleanup removes the `yyc-ai-ui` pin, the seed list runs empty 744 h later; that is not an error
(`--require-while-pinned` fails only while YYC still pins the image). A hashed name that holds different
bytes in two of these sources fails the build, because no single file could serve both tabs. `verify-seeds.mjs`
checks the assembled tree before the push, and the pushed image after it: every seeded file must be present byte for
byte.

## Size

pigeon's emptyDir is 512 MiB (pigeon spec 17.1). The assembled `/app` may take 480 MiB of it, counted as the filesystem
stores it (4 KiB blocks). Measured (`test/sizes-0b3a5c6f5028.json`, the first image's real file sizes): the build and
its 39 seeds take 319.5 MiB, and a build whose every file changed adds 36 MiB (two real builds, 0b3a5c6f and 140ed041,
differ in 58 of 82 files, so that is the usual case). That leaves room for 5 such builds, while the app gets dozens of
commits a week, so spec 17.1's "every build of the last 30 days, or the last 5" does not fit (design gap 1 below). So:

- The served builds and the seeds are mandatory. The last 5 and the last 30 days are kept while the tree fits; over the
  budget `assemble.mjs` prunes the builds YYC has not served, oldest first, those beyond the last 5 before those among
  them, never this build or the one before it. A build YYC never pinned has no tab that needs its chunks; the last 5
  are a margin for a pin that lands just after a run read the pin history. The test "assemble at the real sizes" runs
  8 builds a day apart, each with every file changed and one of them served, and checks every image fits.
- Only if the mandatory part alone is over the budget does the assembly fail, printing what takes the space. It ages
  out by itself (seeds and served builds 744 h after their last serving), so a later run fits again without a change;
  to ship sooner, the lever is the emptyDir `sizeLimit` (with the pod's ephemeral-storage limit, gap 2) and this budget.
- `.br`/`.gz` siblings are written for the pigeon-app builds' files and the unhashed files only. Files kept only for
  seeds are served as they are (pigeon serves the file itself when no sibling exists, pigeon spec 5.2.1). Those are the
  old ui builds' chunks, mostly 21.5 MB entry bundles from before the chunk split, which a running tab never fetches
  again. This saves 110 MiB on the first image.
- The image job checks that `/cp` copies the whole tree as uid 65532 within the initContainer's 32 MiB memory limit
  (`docker run --memory 32m`): under cgroup v2 the page cache the copy dirties is charged to that container.

## Design gaps and deviations

1. **Retention against the emptyDir.** Spec 17.1 keeps every build of the last 30 days or among the last 5; at
   36 MiB a build and dozens of app commits a week, that is far over the 512 MiB emptyDir, and a pigeon-app build
   would fail on the budget within a few commits and stay failed for up to 30 days. Deviation: the 30 days and the
   last 5 are kept only while they fit; the builds YYC served within 720 h, this build and the previous one always.
2. **The pod's ephemeral-storage limit (for the pigeon manifests, WP-31).** Platform 9.4's pigeon Deployment gives the
   pigeon container `ephemeral-storage` limit 128Mi and the initContainer none. kubelet counts a disk-backed emptyDir
   against the pod's ephemeral-storage limit (the sum of the containers' limits, the largest init container's for the
   init phase), so a pod built to the spec is evicted once the initContainer has copied more than 128 MiB, and the
   bundle is 320-480 MiB. The manifest needs the pigeon container's request and limit above the emptyDir plus its own
   use (for example request 576Mi, limit 640Mi; the init phase is then covered too, since the pod's limit is the larger
   of the init and the app phase), or `medium: Memory` with a matching memory limit. This budget follows whichever limit is lowest, less 32 MiB: it stays 480 MiB only if the
   manifest does that.
3. **Pigeon's frame corpus.** Until DE0CH/aviary publishes its first `aviary-*` release (WP-28), `check-frames.sh`
   validates only the harness's fixture frames of aviary main, with a warning, not pigeon's frame corpus (spec 17.1
   step 5). From the first release on, the release branch is taken, and a release without `pigeon-frames.ndjson` or
   `paseo-contract.tgz` is an error. WP-28's acceptance carries the check that this switch happened.

## Files

| File | What |
|---|---|
| `pins.mjs` | the tags of one image (`yyc-ai-ui`, `pigeon-app`) served within a window, from a deploy repository's first-parent history of `yyc-ai/deploy` (any `kustomization.yaml` under it, so the `legacy/` move is followed) |
| `fetch-seeds.mjs` | `/app/dist/_expo/static/**` and `/app/dist/assets/**` of each ui image, via `crane export`. The CI cache is a download cache only: a cached tag is used only at the tag's current digest and with exactly the files and sha256 that the previous image's `meta/seeds.json` records (any job of the branch can write a cache entry, the build job included) |
| `assemble.mjs` | the new export + `sw.js` + kept, restored and seeded files, pruning (by age and under the budget), `.br` (q11) and `.gz` (9) siblings (reused when the bytes are unchanged), the size budget; writes `meta/assets-history.json` (`{file: firstSeenISO}`), `meta/builds.json` and `meta/seeds.json` |
| `verify-seeds.mjs` | every seeded file present byte for byte |
| `a1-ready.sh` | for the cutover runner before A1: `pigeon-app:<A1 sha12>` exists, was built from the A1 commit and seeds `yyc-ai-ui:<A1 sha12>` |
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
   deploy key of DE0CH/YYC. Then it exports `:main` (and the image of any served build `:main` no longer holds), fetches the seeds (checking any cached one against `:main`'s
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
that window (DESIGN 9.6).

- **The A1 build exists.** The A1 commit (WP-56's switch module in `jarvis/router.mjs`) touches no app path, so the
  paths filter includes the ui image's own inputs while `jarvis-image.yml` exists: the A1 commit gets its pigeon-app
  build, which waits for that commit's ui image and seeds it. A `workflow_dispatch` builds the head of main at that
  moment, which need not be the A1 commit, so it is not the way to build A1. Before A1's pins are pushed, the cutover
  runner checks `scripts/pigeon-app/a1-ready.sh <A1 commit>`: the image exists, was built from that commit and seeds
  its own ui image.
- **Nothing else ships.** Pushes to `paseo/**` between A1 and A5 still build images and move `:main` (harmless:
  `:main` is only the next build's merge base), but YYC must not pin them. The check for that belongs to cutover's
  `freeze-check` in DE0CH/YYC (WP-35, an acceptance item there; until it lands nothing enforces it): a change of the `pigeon-app` `newTag` in `yyc-ai/deploy/` between
  A1 and A5 is allowed only to a claudecodeui commit whose diff from the A1 commit touches nothing outside
  `scripts/pigeon-app/` and `.github/workflows/pigeon-app.yml`.
